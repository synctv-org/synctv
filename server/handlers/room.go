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
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/refreshcache0"
	"github.com/zijiren233/gencontainer/synccache"
	"gorm.io/gorm"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
	ErrRoomAlready = errors.New("room already exists")
)

func RoomMe(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	member, err := room.LoadMember(user.ID)
	if err != nil {
		log.Errorf("room me failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.RoomMeResp{
		UserID:           user.ID,
		RoomID:           room.ID,
		JoinAt:           member.CreatedAt.UnixMilli(),
		Status:           member.Status,
		Role:             member.Role,
		Permissions:      member.Permissions,
		AdminPermissions: member.AdminPermissions,
	}))
}

func RoomInfo(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	member, err := room.LoadMember(user.ID)
	if err != nil {
		log.Errorf("room me failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"id":           room.ID,
		"name":         room.Name,
		"needPassword": room.NeedPassword(),
		"creator":      op.GetUserName(room.CreatorID),
		"creatorId":    room.CreatorID,
		"createdAt":    room.CreatedAt.UnixMilli(),
		"status":       room.Status,
		"enabledGuest": room.EnabledGuest(),

		"member": gin.H{
			"id":               user.ID,
			"username":         user.Username,
			"joinAt":           member.CreatedAt.UnixMilli(),
			"status":           member.Status,
			"role":             member.Role,
			"permissions":      member.Permissions,
			"adminPermissions": member.AdminPermissions,
		},
	}))
}

func RoomPiblicSettings(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	ctx.JSON(http.StatusOK, model.NewAPIDataResp(room.Settings))
}

func CreateRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	if settings.DisableCreateRoom.Get() && !user.IsAdmin() {
		log.Error("create room is disabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("create room is disabled"))
		return
	}

	req := model.CreateRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("create room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	room, err := user.CreateRoom(req.RoomName, req.Password, db.WithSettingHidden(req.Settings.Hidden))
	if err != nil {
		log.Errorf("create room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusCreated, model.NewAPIDataResp(gin.H{
		"roomId": room.Value().ID,
		"status": room.Value().Status,
	}))
}

var roomHotCache = refreshcache0.NewRefreshCache[[]*model.RoomListResp](func(context.Context) ([]*model.RoomListResp, error) {
	rooms := make([]*model.RoomListResp, 0)
	op.RangeRoomCache(func(key string, value *synccache.Entry[*op.Room]) bool {
		v := value.Value()
		if !v.Settings.Hidden && v.IsActive() && !v.HubIsNotInited() {
			rooms = append(rooms, &model.RoomListResp{
				RoomID:       v.ID,
				RoomName:     v.Name,
				ViewerCount:  v.ViewerCount(),
				NeedPassword: v.NeedPassword(),
				Creator:      op.GetUserName(v.CreatorID),
				CreatorID:    v.CreatorID,
				CreatedAt:    v.CreatedAt.UnixMilli(),
			})
		}
		return true
	})

	slices.SortStableFunc(rooms, func(a, b *model.RoomListResp) int {
		if a.ViewerCount == b.ViewerCount {
			if a.RoomName == b.RoomName {
				return 0
			}
			if natural.Less(a.RoomName, b.RoomName) {
				return -1
			}
			return 1
		} else if a.ViewerCount > b.ViewerCount {
			return -1
		}
		return 1
	})

	return rooms, nil
}, time.Second*3)

func RoomHotList(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get room hot list failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	r, err := roomHotCache.Get(ctx)
	if err != nil {
		log.Errorf("get room hot list failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": len(r),
		"list":  utils.GetPageItems(r, page, pageSize),
	}))
}

func RoomList(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	page, pageSize, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get room list failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	scopes := []func(db *gorm.DB) *gorm.DB{
		func(db *gorm.DB) *gorm.DB {
			return db.InnerJoins("JOIN room_settings ON rooms.id = room_settings.id")
		},
		db.WhereRoomSettingWithoutHidden(),
		db.WhereStatus(dbModel.RoomStatusActive),
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.Errorf("get room list failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrRoomsIDLike(keyword, ids, keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "creator":
			ids, err := db.GerUsersIDByUsernameLike(keyword)
			if err != nil {
				log.Errorf("get room list failed: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
				return
			}
			scopes = append(scopes, db.WhereCreatorIDIn(ids))
		case "id":
			scopes = append(scopes, db.WhereRoomsIDLike(keyword))
		}
	}

	total, err := db.GetAllRoomsCount(scopes...)
	if err != nil {
		log.Errorf("get room list failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	desc := ctx.DefaultQuery("order", "desc") == "desc"
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
		log.Errorf("get room list failed: not support sort")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("not support sort"))
		return
	}

	list, err := genRoomListResp(append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		log.Errorf("get room list failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"total": total,
		"list":  list,
	}))
}

func genRoomListResp(scopes ...func(db *gorm.DB) *gorm.DB) ([]*model.RoomListResp, error) {
	rs, err := db.GetAllRooms(scopes...)
	if err != nil {
		return nil, err
	}
	resp := make([]*model.RoomListResp, len(rs))
	for i, r := range rs {
		resp[i] = &model.RoomListResp{
			RoomID:       r.ID,
			RoomName:     r.Name,
			ViewerCount:  op.ViewerCount(r.ID),
			NeedPassword: len(r.HashedPassword) != 0,
			CreatorID:    r.CreatorID,
			Creator:      op.GetUserName(r.CreatorID),
			CreatedAt:    r.CreatedAt.UnixMilli(),
			Status:       r.Status,
		}
	}
	return resp, nil
}

func genJoinedRoomListResp(scopes ...func(db *gorm.DB) *gorm.DB) ([]*model.JoinedRoomResp, error) {
	rs, err := db.GetAllRooms(scopes...)
	if err != nil {
		return nil, err
	}
	resp := make([]*model.JoinedRoomResp, len(rs))
	for i, r := range rs {
		if len(r.RoomMembers) == 0 {
			return nil, fmt.Errorf("room %s load member failed", r.ID)
		}
		resp[i] = &model.JoinedRoomResp{
			RoomListResp: model.RoomListResp{
				RoomID:       r.ID,
				RoomName:     r.Name,
				ViewerCount:  op.ViewerCount(r.ID),
				NeedPassword: len(r.HashedPassword) != 0,
				CreatorID:    r.CreatorID,
				Creator:      op.GetUserName(r.CreatorID),
				CreatedAt:    r.CreatedAt.UnixMilli(),
				Status:       r.Status,
			},
			MemberStatus: r.RoomMembers[0].Status,
			MemberRole:   r.RoomMembers[0].Role,
		}
	}
	return resp, nil
}

func CheckRoom(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)
	roomID, err := middlewares.GetRoomIDFromContext(ctx)
	if err != nil {
		log.Errorf("check room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(roomID)
	if err != nil {
		log.Errorf("check room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewAPIErrorResp(err))
		return
	}
	room := roomE.Value()

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&model.CheckRoomResp{
		Name:         room.Name,
		Status:       room.Status,
		CreatorID:    room.CreatorID,
		Creator:      op.GetUserName(room.CreatorID),
		NeedPassword: room.NeedPassword(),
		ViewerCount:  op.ViewerCount(room.ID),
		EnabledGuest: room.EnabledGuest(),
	}))
}

func LoginRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.LoginRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("login room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	roomE, err := op.LoadOrInitRoomByID(req.RoomID)
	if err != nil {
		log.Errorf("login room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}
	room := roomE.Value()

	if room.IsBanned() {
		log.Warn("login room failed: room is banned")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewAPIErrorStringResp("room is banned"))
		return
	}

	if room.IsPending() {
		log.Warn("login room failed: room is pending, please wait for admin to approve")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewAPIErrorStringResp("room is pending, please wait for admin to approve"))
		return
	}

	if member, err := room.LoadMember(user.ID); err == nil {
		ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
			"status":           member.Status,
			"role":             member.Role,
			"permissions":      member.Permissions,
			"adminPermissions": member.AdminPermissions,
		}))
		return
	}

	if !user.IsAdmin() && !user.IsRoomAdmin(room) && !room.CheckPassword(req.Password) {
		log.Warn("login room failed: password error")
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewAPIErrorStringResp("password error"))
		return
	}

	member, err := room.LoadOrCreateMember(user.ID)
	if err != nil {
		if errors.Is(err, db.NotFoundError(db.ErrRoomMemberNotFound)) {
			log.Warn("login room failed: room was disabled join new user")
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorResp(
					errors.New("this room was disabled join new user"),
				),
			)
			return
		}
		log.Errorf("login room failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"status":           member.Status,
		"role":             member.Role,
		"permissions":      member.Permissions,
		"adminPermissions": member.AdminPermissions,
	}))
}

func CheckRoomPassword(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.CheckRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("check room password failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"valid": room.CheckPassword(req.Password),
	}))
}

func DeleteRoom(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry)
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	if err := user.DeleteRoom(room); err != nil {
		log.Errorf("delete room failed: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorResp(
					fmt.Errorf("delete room failed: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SetRoomPassword(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.SetRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("set room password failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := user.SetRoomPassword(room, req.Password); err != nil {
		log.Errorf("set room password failed: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorResp(
					fmt.Errorf("set room password failed: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RoomSetting(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	// user := ctx.MustGet("user").(*op.UserEntry)

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(room.Settings))
}

func SetRoomSetting(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.SetRoomSettingReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("set room setting failed: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if err := user.UpdateRoomSettings(room, req); err != nil {
		log.Errorf("set room setting failed: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewAPIErrorResp(
					fmt.Errorf("set room setting failed: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
