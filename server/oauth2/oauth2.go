package auth

import (
	"sync"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/zijiren233/gencontainer/vec"
	"golang.org/x/exp/maps"
)

var (
	oauth2EnabledCache []provider.OAuth2Provider
	oauth2EnabledOnce  sync.Once
)

func OAuth2EnabledApi(ctx *gin.Context) {
	oauth2EnabledOnce.Do(func() {
		oauth2EnabledCache = maps.Keys(providers.EnabledProvider())
		a := vec.New[provider.OAuth2Provider](vec.WithCmpEqual[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 == v2
		}), vec.WithCmpLess[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 < v2
		}))
		a.Push(oauth2EnabledCache...).SortStable().Slice()
	})
	ctx.JSON(200, gin.H{
		"enabled": oauth2EnabledCache,
	})
}
