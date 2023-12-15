package vendors

import (
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"golang.org/x/exp/maps"
)

func Backends(ctx *gin.Context) {
	var backends []string
	switch ctx.Param("vendor") {
	case dbModel.VendorBilibili:
		backends = maps.Keys(vendor.LoadClients().BilibiliClients())
	case dbModel.VendorAlist:
		backends = maps.Keys(vendor.LoadClients().AlistClients())
	case dbModel.VendorEmby:
		backends = maps.Keys(vendor.LoadClients().EmbyClients())
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid vendor name"))
		return
	}
	ctx.JSON(http.StatusOK, model.NewApiDataResp(backends))
}
