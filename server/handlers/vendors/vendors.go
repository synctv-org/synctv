package vendors

import (
	"context"
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendoralist"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendorbilibili"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendoremby"
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("invalid vendor name"))
		return
	}
	ctx.JSON(http.StatusOK, model.NewAPIDataResp(backends))
}

type VendorService interface {
	ListDynamicMovie(ctx context.Context, reqUser *op.User, subPath string, page, _max int) (*model.MovieList, error)
	ProxyMovie(ctx *gin.Context)
	GenMovieInfo(ctx context.Context, reqUser *op.User, userAgent, userToken string) (*dbModel.Movie, error)
}

func NewVendorService(room *op.Room, movie *op.Movie) (VendorService, error) {
	switch movie.VendorInfo.Vendor {
	case dbModel.VendorBilibili:
		return vendorbilibili.NewBilibiliVendorService(room, movie)
	case dbModel.VendorAlist:
		return vendoralist.NewAlistVendorService(room, movie)
	case dbModel.VendorEmby:
		return vendoremby.NewEmbyVendorService(room, movie)
	default:
		return nil, fmt.Errorf("vendor %s not support", movie.VendorInfo.Vendor)
	}
}
