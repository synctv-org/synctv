package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/server/model"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/gencontainer/vec"
)

var (
	Oauth2EnabledCache = refreshcache.NewRefreshCache[[]provider.OAuth2Provider](func() ([]provider.OAuth2Provider, error) {
		a := vec.New[provider.OAuth2Provider](vec.WithCmpEqual[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 == v2
		}), vec.WithCmpLess[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 < v2
		}))
		providers.EnabledProvider().Range(func(key provider.OAuth2Provider, value provider.ProviderInterface) bool {
			a.Push(key)
			return true
		})
		return a.SortStable().Slice(), nil
	}, time.Hour)
)

func OAuth2EnabledApi(ctx *gin.Context) {
	data, err := Oauth2EnabledCache.Get()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(200, gin.H{
		"enabled": data,
	})
}
