package auth

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
)

func OAuth2EnabledAPI(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	data, err := bootstrap.Oauth2EnabledCache.Get(ctx)
	if err != nil {
		log.Errorf("failed to get oauth2 enabled: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(200, gin.H{
		"enabled": data,
	})
}

func OAuth2SignupEnabledAPI(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	oauth2SignupEnabled, err := bootstrap.Oauth2SignupEnabledCache.Get(ctx)
	if err != nil {
		log.Errorf("failed to get oauth2 signup enabled: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.JSON(200, gin.H{
		"signupEnabled": oauth2SignupEnabled,
	})
}
