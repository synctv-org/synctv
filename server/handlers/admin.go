package handlers

import (
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

func EditAdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)

	req := model.AdminSettingsReq{}
	if err := req.Decode(ctx); err != nil {
		ctx.AbortWithError(http.StatusBadRequest, err)
		return
	}

	for k, v := range req {
		t, ok := op.GetSettingType(k)
		if !ok {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(fmt.Sprintf("setting %s not found", k)))
			return
		}
		switch t {
		case dbModel.SettingTypeBool:
			b, ok := v.(bool)
			if !ok {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(fmt.Sprintf("setting %s is not bool", k)))
				return
			}
			op.BoolSettings[k].Set(b)
		}
	}

	ctx.Status(http.StatusNoContent)
}

func AdminSettings(ctx *gin.Context) {
	// user := ctx.MustGet("user").(*op.User)
	group := ctx.Query("group")
	if group == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("group is required"))
		return
	}

	s := op.GetSettingByGroup(dbModel.SettingGroup(group))
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
