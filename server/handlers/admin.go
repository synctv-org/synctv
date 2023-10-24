package handlers

import (
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

func AdminSettings(ctx *gin.Context) {
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
			op.BoolSettings[k].Set(v == "1")
		}
	}
}
