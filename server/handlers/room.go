package handlers

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/vec"
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
	user := ctx.MustGet("user").(*op.User)
	req := model.CreateRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := user.CreateRoom(req.RoomName, req.Password, db.WithSetting(req.Setting))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, _ := op.LoadOrInitRoom(r)

	token, err := middlewares.NewAuthRoomToken(user, room)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusCreated, model.NewApiDataResp(gin.H{
		"roomId": room.ID,
		"token":  token,
	}))
}

func RoomList(ctx *gin.Context) {
	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	resp := make([]*model.RoomListResp, 0, pageSize)

	var desc = ctx.DefaultQuery("sort", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.Paginate(page, pageSize),
	}

	total := 0

	switch ctx.DefaultQuery("order", "peopleNum") {
	case "peopleNum":
		r := op.GetAllRoomsInCacheWithoutHidden()
		rs := vec.New[*model.RoomListResp](vec.WithCmpLess[*model.RoomListResp](func(v1, v2 *model.RoomListResp) bool {
			return v1.PeopleNum < v2.PeopleNum
		}), vec.WithCmpEqual[*model.RoomListResp](func(v1, v2 *model.RoomListResp) bool {
			return v1.PeopleNum == v2.PeopleNum
		}))
		for _, v := range r {
			rs.Push(&model.RoomListResp{
				RoomId:       v.ID,
				RoomName:     v.Name,
				PeopleNum:    v.ClientNum(),
				NeedPassword: v.NeedPassword(),
				Creator:      op.GetUserName(v.Room.CreatorID),
				CreatedAt:    v.Room.CreatedAt.UnixMilli(),
			})
		}
		rs.SortStable()
		if desc {
			rs.Reverse()
		}
		total = rs.Len()
		resp = utils.GetPageItems(rs.Slice(), page, pageSize)
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
		resp = genRoomsResp(resp, scopes...)
	case "roomName":
		if desc {
			scopes = append(scopes, db.OrderByDesc("name"))
		} else {
			scopes = append(scopes, db.OrderByAsc("name"))
		}
		resp = genRoomsResp(resp, scopes...)
	case "roomId":
		if desc {
			scopes = append(scopes, db.OrderByIDDesc)
		} else {
			scopes = append(scopes, db.OrderByIDAsc)
		}
		resp = genRoomsResp(resp, scopes...)
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support order"))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": total,
		"list":  resp,
	}))
}

func genRoomsResp(resp []*model.RoomListResp, scopes ...func(db *gorm.DB) *gorm.DB) []*model.RoomListResp {
	var clientNum int64
	for _, r := range db.GetAllRooms(scopes...) {
		room, err := op.LoadRoomByID(r.ID)
		if err != nil {
			clientNum = 0
		} else {
			clientNum = room.ClientNum()
		}

		resp = append(resp, &model.RoomListResp{
			RoomId:       r.ID,
			RoomName:     r.Name,
			PeopleNum:    clientNum,
			NeedPassword: len(r.HashedPassword) != 0,
			Creator:      op.GetUserName(r.CreatorID),
			CreatedAt:    r.CreatedAt.UnixMilli(),
		})
	}
	return resp
}

func CheckRoom(ctx *gin.Context) {
	id, err := strconv.Atoi(ctx.Query("roomId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	r, err := db.GetRoomByID(uint(id))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	var peopleNum int64

	room, err := op.LoadRoomByID(r.ID)
	if err == nil {
		peopleNum = room.ClientNum()
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"peopleNum":    peopleNum,
		"needPassword": r.NeedPassword(),
	}))
}

func LoginRoom(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := model.LoginRoomReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := middlewares.AuthRoomWithPassword(user, req.RoomId, req.Password)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
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

func DeleteRoom(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	if !user.HasPermission(room, dbModel.CanDeleteRoom) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("you don't have permission to delete room"))
		return
	}

	err := op.DeleteRoom(room)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SetRoomPassword(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	if !user.HasPermission(room, dbModel.CanSetRoomPassword) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("you don't have permission to set room password"))
		return
	}

	req := model.SetRoomPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user)
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
	room := ctx.MustGet("room").(*op.Room)
	// user := ctx.MustGet("user").(*op.User)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"hidden":       room.Settings.Hidden,
		"needPassword": room.NeedPassword(),
	}))
}
