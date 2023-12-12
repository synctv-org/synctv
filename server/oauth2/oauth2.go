package auth

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/server/model"
)

func OAuth2EnabledApi(ctx *gin.Context) {
	data, err := bootstrap.Oauth2EnabledCache.Get(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(200, gin.H{
		"enabled": data,
	})
}
