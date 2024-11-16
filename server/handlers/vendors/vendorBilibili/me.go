package vendorbilibili

import (
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/bilibili"
)

type BilibiliMeResp = model.VendorMeResp[*bilibili.UserInfoResp]

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	bucd, err := user.BilibiliCache().Get(ctx)
	if err != nil {
		if errors.Is(err, db.NotFoundError(db.ErrVendorNotFound)) {
			ctx.JSON(http.StatusOK, model.NewAPIDataResp(&BilibiliMeResp{
				IsLogin: false,
			}))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	if len(bucd.Cookies) == 0 {
		ctx.JSON(http.StatusOK, model.NewAPIDataResp(&BilibiliMeResp{
			IsLogin: false,
		}))
		return
	}
	resp, err := vendor.LoadBilibiliClient(bucd.Backend).UserInfo(ctx, &bilibili.UserInfoReq{
		Cookies: utils.HTTPCookieToMap(bucd.Cookies),
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(&BilibiliMeResp{
		IsLogin: resp.IsLogin,
		Info:    resp,
	}))
}
