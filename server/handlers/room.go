package handlers

import (
	"context"
	"errors"
	"fmt"
	"net/http"
	"slices"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/maruel/natural"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/gencontainer/synccache"
	"gorm.io/gorm"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
	ErrRoomAlready = errors.New("room already exists")
)

type FormatErrNotSupportPosition string

func (e FormatErrNotSupportPosition) Error() string {
	return fmt.Sprintf("not support position %s", string(e))
}

func CreateRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	if settings.DisableCreateRoom.Get() && !user.IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("create room is disabled"))
		return
	}

	req := model.CreateRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := user.CreateRoom(req.RoomName, req.Password, db.WithSetting(req.Setting))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthRoomToken(user, room.Value())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusCreated, model.NewApiDataResp(gin.H{
		"roomId": room.Value().ID,
		"token":  token,
	}))
}

var roomHotCache = refreshcache.NewRefreshCache[[]*model.RoomListResp](func(context.Context, ...any) ([]*model.RoomListResp, error) {
	rooms := make([]*model.RoomListResp, 0)
	op.RangeRoomCache(func(key string, value *synccache.Entry[*op.Room]) bool {
		v := value.Value()
		if !v.Settings.Hidden {
			rooms = append(rooms, &model.RoomListResp{
				RoomId:       v.ID,
				RoomName:     v.Name,
				PeopleNum:    v.PeopleNum(),
				NeedPassword: v.NeedPassword(),
				Creator:      op.GetUserName(v.CreatorID),
				CreatedAt:    v.CreatedAt.UnixMilli(),
			})
		}
		return true
	})

	slices.SortStableFunc(rooms, func(a, b *model.RoomListResp) int {
		if a.PeopleNum == b.PeopleNum {
			if a.RoomName == b.RoomName {
				return 0
			}
			if natural.Less(a.RoomName, b.RoomName) {
				return -1
			} else {
				return 1
			}
		} else if a.PeopleNum > b.PeopleNum {
			return -1
		} else {
			return 1
		}
	})

	return rooms, nil
}, time.Second*3)

func RoomHotList(ctx *gin.Context) {
	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := roomHotCache.Get(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": len(r),
		"list":  utils.GetPageItems(r, page, pageSize),
	}))
}

func RoomList(ctx *gin.Context) {
	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.WhereRoomSettingWithoutHidden(),
		db.WhereStatus(dbModel.RoomStatusActive),
	}

	switch ctx.DefaultQuery("sort", "name") {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("name"))
		} else {
			scopes = append(scopes, db.OrderByAsc("name"))
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, db.GerUsersIDByUsernameLike(keyword), keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "creator":
			scopes = append(scopes, db.WhereCreatorIDIn(db.GerUsersIDByUsernameLike(keyword)))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRoomsCount(scopes...),
		"list":  genRoomListResp(append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func genRoomListResp(scopes ...func(db *gorm.DB) *gorm.DB) []*model.RoomListResp {
	rs := db.GetAllRooms(scopes...)
	resp := make([]*model.RoomListResp, len(rs))
	for i, r := range rs {
		resp[i] = &model.RoomListResp{
			RoomId:       r.ID,
			RoomName:     r.Name,
			PeopleNum:    op.PeopleNum(r.ID),
			NeedPassword: len(r.HashedPassword) != 0,
			CreatorID:    r.CreatorID,
			Creator:      op.GetUserName(r.CreatorID),
			CreatedAt:    r.CreatedAt.UnixMilli(),
			Status:       r.Status,
		}
	}
	return resp
}

func CheckRoom(ctx *gin.Context) {
	r, err := db.GetRoomByID(ctx.Query("roomId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"peopleNum":    op.PeopleNum(r.ID),
		"needPassword": r.NeedPassword(),
		"creator":      op.GetUserName(r.CreatorID),
	}))
}

func LoginRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.LoginRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := op.LoadOrInitRoomByID(req.RoomId)
	if err != nil {
		if err == op.ErrRoomBanned || err == op.ErrRoomPending {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	if room.Value().CreatorID != user.ID && !room.Value().CheckPassword(req.Password) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("password error"))
		return
	}

	token, err := middlewares.NewAuthRoomToken(user, room.Value())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"roomId": room.Value().ID,
		"token":  token,
	}))
}

func DeleteRoom(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry)
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	if err := user.DeleteRoom(room); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SetRoomPassword(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.SetRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetRoomPassword(room, req.Password); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthRoomToken(user, room)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"roomId": room.ID,
		"token":  token,
	}))
}

func RoomSetting(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	// user := ctx.MustGet("user").(*op.UserEntry)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(room.Settings))
}

func SetRoomSetting(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.SetRoomSettingReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetRoomSetting(room, dbModel.RoomSettings(req)); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomUsers(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("order", "desc") == "desc"

	preloadScopes := []func(db *gorm.DB) *gorm.DB{db.WhereRoomID(room.ID)}
	scopes := []func(db *gorm.DB) *gorm.DB{}

	switch ctx.DefaultQuery("status", "active") {
	case "pending":
		preloadScopes = append(preloadScopes, db.WhereRoomUserStatus(dbModel.RoomUserStatusPending))
	case "banned":
		preloadScopes = append(preloadScopes, db.WhereRoomUserStatus(dbModel.RoomUserStatusBanned))
	case "active":
		preloadScopes = append(preloadScopes, db.WhereRoomUserStatus(dbModel.RoomUserStatusActive))
	}

	switch ctx.DefaultQuery("sort", "name") {
	case "join":
		if desc {
			preloadScopes = append(preloadScopes, db.OrderByCreatedAtDesc)
		} else {
			preloadScopes = append(preloadScopes, db.OrderByCreatedAtAsc)
		}
	case "name":
		if desc {
			scopes = append(scopes, db.OrderByDesc("username"))
		} else {
			scopes = append(scopes, db.OrderByAsc("username"))
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support sort"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, id
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereUsernameLikeOrIDIn(keyword, db.GerUsersIDByIDLike(keyword)))
		case "name":
			scopes = append(scopes, db.WhereUsernameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDIn(db.GerUsersIDByIDLike(keyword)))
		}
	}
	scopes = append(scopes, db.PreloadRoomUserRelations(preloadScopes...))

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllUserCount(scopes...),
		"list":  genRoomUserListResp(db.GetAllUsers(append(scopes, db.Paginate(page, pageSize))...)),
	}))
}
