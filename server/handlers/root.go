package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

func RootAddAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.IDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.ID == user.ID {
		log.Errorf("cannot add yourself")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("cannot add yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("failed to load user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorStringResp("user not found"))
		return
	}
	if u.Value().IsAdmin() {
		log.Errorf("user is already admin")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("user is already admin"))
		return
	}

	if err := u.Value().SetAdminRole(); err != nil {
		log.Errorf("failed to set role: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func RootDeleteAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry)
	log := ctx.MustGet("log").(*logrus.Entry)

	req := model.IDReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.ID == user.Value().ID {
		log.Errorf("cannot remove yourself")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("cannot remove yourself"))
		return
	}
	u, err := op.LoadOrInitUserByID(req.ID)
	if err != nil {
		log.Errorf("failed to load user: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorStringResp("user not found"))
		return
	}
	if u.Value().IsRoot() {
		log.Errorf("cannot remove root")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("cannot remove root"))
		return
	}

	if err := u.Value().SetUserRole(); err != nil {
		log.Errorf("failed to set role: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
