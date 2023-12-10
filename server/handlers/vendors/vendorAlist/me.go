package vendorAlist

import (
	"errors"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/alist"
)

type AlistMeResp = model.VendorMeResp[*alist.MeResp]

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	cli := vendor.AlistClient(ctx.Query("backend"))

	authorizationI, err := user.Cache.LoadOrStoreWithDynamicFunc("alist_authorization", initAlistAuthorizationCacheWithUserID(ctx, cli, user.ID), time.Hour*24)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusOK, model.NewApiDataResp(&AlistMeResp{
				IsLogin: false,
			}))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	cache, ok := authorizationI.(*alistCache)
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	resp, err := cli.Me(ctx, &alist.MeReq{
		Host:  cache.Host,
		Token: cache.Token,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&AlistMeResp{
		IsLogin: false,
		Info:    resp,
	}))

}
