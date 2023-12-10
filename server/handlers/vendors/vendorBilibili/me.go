package vendorBilibili

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/bilibili"
)

type BilibiliMeResp = model.VendorMeResp[*bilibili.UserInfoResp]

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	v, err := db.GetBilibiliVendor(user.ID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusOK, model.NewApiDataResp(&BilibiliMeResp{
				IsLogin: false,
			}))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	if len(v.Cookies) == 0 {
		ctx.JSON(http.StatusOK, model.NewApiDataResp(&BilibiliMeResp{
			IsLogin: false,
		}))
		return
	}
	resp, err := vendor.BilibiliClient("").UserInfo(ctx, &bilibili.UserInfoReq{
		Cookies: v.Cookies,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(&BilibiliMeResp{
		IsLogin: resp.IsLogin,
		Info:    resp,
	}))
}
