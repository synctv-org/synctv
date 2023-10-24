package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/model"
)

func EditAdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)

	req := model.AdminSettingsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	for k, v := range req {
		err := settings.SetValue(k, v)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	group := ctx.Param("group")
	if group == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group is required"))
		return
	}

	s, ok := settings.GroupSettings[dbModel.SettingGroup(group)]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group not found"))
		return
	}
	resp := make(gin.H, len(s))
	for _, v := range s {
		i, err := v.Interface()
		if err != nil {
			ctx.AbortWithError(http.StatusInternalServerError, err)
			return
		}
		resp[v.Name()] = i
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}

func AddAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("permission denied"))
		return
	}

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetRole(dbModel.RoleAdmin); err != nil {
		ctx.AbortWithError(http.StatusInternalServerError, err)
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DeleteAdmin(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("permission denied"))
		return
	}

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetRole(dbModel.RoleUser); err != nil {
		ctx.AbortWithError(http.StatusInternalServerError, err)
		return
	}

	ctx.Status(http.StatusNoContent)
}
