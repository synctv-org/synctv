package vendors

import (
	"context"
	"fmt"
	"maps"
	"net/http"
	"slices"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendoralist"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendorbilibili"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendoremby"
	"github.com/synctv-org/synctv/server/model"
)

func Backends(ctx *gin.Context) {
	var backends []string
	switch ctx.Param("vendor") {
	case dbModel.VendorBilibili:
		backends = slices.Collect(maps.Keys(vendor.LoadClients().BilibiliClients()))
	case dbModel.VendorAlist:
		backends = slices.Collect(maps.Keys(vendor.LoadClients().AlistClients()))
	case dbModel.VendorEmby:
		backends = slices.Collect(maps.Keys(vendor.LoadClients().EmbyClients()))
	default:
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("invalid vendor name"),
		)

		return
	}

	ctx.JSON(http.StatusOK, model.NewAPIDataResp(backends))
}

type VendorService interface {
	ListDynamicMovie(
		ctx context.Context,
		reqUser *op.User,
		subPath, keyword string,
		page, _max int,
	) (*model.MovieList, error)
	ProxyMovie(ctx *gin.Context)
	GenMovieInfo(
		ctx context.Context,
		reqUser *op.User,
		userAgent, userToken string,
	) (*dbModel.Movie, error)
}

type VendorDanmuService interface {
	StreamDanmu(ctx context.Context, handler func(danmu string) error) error
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
