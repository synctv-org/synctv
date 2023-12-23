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
	user := ctx.MustGet("user").(*op.User)

	eucd, err := user.EmbyCache().Get(ctx)
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
