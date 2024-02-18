package auth

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/server/model"
)

func OAuth2EnabledApi(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	data, err := bootstrap.Oauth2EnabledCache.Get(ctx)
	if err != nil {
		log.Errorf("failed to get oauth2 enabled: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(200, gin.H{
		"enabled": data,
	})
}
