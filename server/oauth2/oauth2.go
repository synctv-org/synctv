package auth

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"golang.org/x/exp/maps"
)

func OAuth2EnabledApi(ctx *gin.Context) {
	ctx.JSON(200, gin.H{
		"enabled": maps.Keys(providers.EnabledProvider()),
	})
}
