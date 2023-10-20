package auth

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/exp/maps"
)

func OAuth2EnabledApi(ctx *gin.Context) {
	ctx.JSON(200, gin.H{
		"enabled": maps.Keys(provider.EnabledProvider()),
	})
}
