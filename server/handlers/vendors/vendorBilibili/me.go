package vendorBilibili

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/bilibili"
)

type BilibiliMeResp = model.VendorMeResp[*bilibili.UserInfoResp]

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	v, err := db.GetVendorByUserIDAndVendor(user.ID, dbModel.StreamingVendorBilibili)
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
		Cookies: utils.HttpCookieToMap(v.Cookies),
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
