package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
	"gorm.io/gorm"
)

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&model.UserInfoResp{
		ID:        user.ID,
		Username:  user.Username,
		Role:      user.Role,
		CreatedAt: user.CreatedAt.UnixMilli(),
	}))
}

func LogoutUser(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	err := op.DeleteUserByID(user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func UserRooms(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	order := ctx.Query("order")
	if order == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("order is required"))
		return
	}
	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("sort", "desc") == "desc"

	// search mode, all, name
	var search = ctx.DefaultQuery("search", "all")

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.WhereCreatorID(user.ID),
	}

	switch order {
	case "createdAt":
		if desc {
			scopes = append(scopes, db.OrderByCreatedAtDesc)
		} else {
			scopes = append(scopes, db.OrderByCreatedAtAsc)
		}
		if keyword := ctx.Query("keyword"); keyword != "" {
			switch search {
			case "all":
				scopes = append(scopes, db.WhereRoomNameLikeOrCreatorIn(keyword, db.GerUsersIDByUsernameLike(keyword)))
			case "name":
				scopes = append(scopes, db.WhereRoomNameLike(keyword))
			}
		}
	case "roomName":
		if desc {
			scopes = append(scopes, db.OrderByDesc("name"))
		} else {
			scopes = append(scopes, db.OrderByAsc("name"))
		}
		if keyword := ctx.Query("keyword"); keyword != "" {
			switch search {
			case "all":
				scopes = append(scopes, db.WhereRoomNameLikeOrCreatorIn(keyword, db.GerUsersIDByUsernameLike(keyword)))
			case "name":
				scopes = append(scopes, db.WhereRoomNameLike(keyword))
			}
		}
	case "roomId":
		if desc {
			scopes = append(scopes, db.OrderByIDDesc)
		} else {
			scopes = append(scopes, db.OrderByIDAsc)
		}
		if keyword := ctx.Query("keyword"); keyword != "" {
			switch search {
			case "all":
				scopes = append(scopes, db.WhereRoomNameLikeOrCreatorIn(keyword, db.GerUsersIDByUsernameLike(keyword)))
			case "name":
				scopes = append(scopes, db.WhereRoomNameLike(keyword))
			}
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support order"))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRoomsWithoutHiddenCount(scopes...),
		"list":  genRoomListResp(append(scopes, db.Paginate(page, pageSize))...),
	}))
}

func SetUsername(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	var req model.SetUsernameReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.SetUsername(req.Username)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
