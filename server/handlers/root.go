package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

func AddAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Id == user.ID {
		log.Errorf("cannot add yourself")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot add yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.Id)
	if err != nil {
		log.Errorf("failed to load user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("user not found"))
		return
	}
	if u.Value().IsAdmin() {
		log.Errorf("user is already admin")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("user is already admin"))
		return
	}

	if err := u.Value().SetAdminRole(); err != nil {
		log.Errorf("failed to set role: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DeleteAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Id == user.Value().ID {
		log.Errorf("cannot remove yourself")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot remove yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.Id)
	if err != nil {
		log.Errorf("failed to load user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("user not found"))
		return
	}
	if u.Value().IsRoot() {
		log.Errorf("cannot remove root")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("cannot remove root"))
		return
	}

	if err := u.Value().SetUserRole(); err != nil {
		log.Errorf("failed to set role: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
