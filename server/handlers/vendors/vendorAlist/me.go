package vendorAlist

import (
	"errors"
	"net/http"

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

	aucd, err := user.AlistCache().Get(ctx, ctx.Query("backend"))
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

	resp, err := cli.Me(ctx, &alist.MeReq{
		Host:  aucd.Host,
		Token: aucd.Token,
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
