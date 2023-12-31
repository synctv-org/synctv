package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

func AddAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Id == user.ID {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot add yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("user not found"))
		return
	}
	if u.Value().IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is already admin"))
		return
	}

	if err := u.Value().SetRole(dbModel.RoleAdmin); err != nil {
		ctx.AbortWithError(http.StatusInternalServerError, err)
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DeleteAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry)

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Id == user.Value().ID {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot remove yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("user not found"))
		return
	}
	if u.Value().IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot remove root"))
		return
	}

	if err := u.Value().SetRole(dbModel.RoleUser); err != nil {
		ctx.AbortWithError(http.StatusInternalServerError, err)
		return
	}

	ctx.Status(http.StatusNoContent)
}
