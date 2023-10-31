package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
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

	page, pageSize, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var desc = ctx.DefaultQuery("sort", "desc") == "desc"

	scopes := []func(db *gorm.DB) *gorm.DB{
		db.WhereCreatorID(user.ID),
	}

	switch ctx.DefaultQuery("status", "active") {
	case "active":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusActive))
	case "pending":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusPending))
	case "banned":
		scopes = append(scopes, db.WhereStatus(dbModel.RoomStatusBanned))
	}

	switch ctx.DefaultQuery("order", "name") {
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support order"))
		return
	}

	if keyword := ctx.Query("keyword"); keyword != "" {
		// search mode, all, name, creator
		switch ctx.DefaultQuery("search", "all") {
		case "all":
			scopes = append(scopes, db.WhereRoomNameLikeOrCreatorInOrIDLike(keyword, db.GerUsersIDByUsernameLike(keyword), keyword))
		case "name":
			scopes = append(scopes, db.WhereRoomNameLike(keyword))
		case "id":
			scopes = append(scopes, db.WhereIDLike(keyword))
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total": db.GetAllRooms(scopes...),
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
