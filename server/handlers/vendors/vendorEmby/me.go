package vendorEmby

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/emby"
)

type EmbyMeResp = model.VendorMeResp[*emby.SystemInfoResp]

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	serverID := ctx.Query("serverID")
	if serverID == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(errors.New("serverID is required")))
		return

	}

	eucd, err := user.EmbyCache().LoadOrStore(ctx, serverID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusBadRequest, model.NewApiErrorStringResp("emby server not found"))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	data, err := vendor.LoadEmbyClient(eucd.Backend).GetSystemInfo(ctx, &emby.SystemInfoReq{
		Host:  eucd.Host,
		Token: eucd.ApiKey,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&EmbyMeResp{
		IsLogin: true,
		Info:    data,
	}))
}

type EmbyBindsResp []*struct {
	ServerID string `json:"serverID"`
	Host     string `json:"host"`
}

func Binds(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	ev, err := db.GetEmbyVendors(user.ID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusOK, model.NewApiDataResp(&EmbyMeResp{
				IsLogin: false,
			}))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	var resp EmbyBindsResp = make(EmbyBindsResp, len(ev))
	for i, v := range ev {
		resp[i] = &struct {
			ServerID string "json:\"serverID\""
			Host     string "json:\"host\""
		}{
			ServerID: v.ServerID,
			Host:     v.Host,
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
