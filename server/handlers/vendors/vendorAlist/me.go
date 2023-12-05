package vendorAlist

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/alist"
)

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)
	v, err := db.FirstOrCreateVendorByUserIDAndVendor(user.ID, dbModel.StreamingVendorAlist)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	resp, err := vendor.AlistClient("").Me(ctx, &alist.MeReq{
		Host:  v.Host,
		Token: v.Authorization,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
