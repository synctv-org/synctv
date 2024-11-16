package auth

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/server/middlewares"
)

func Init(e *gin.Engine) {
	{
		oauth2 := e.Group("/oauth2")
		needAuthOauth2 := oauth2.Group("")
		needAuthOauth2.Use(middlewares.AuthUserMiddleware)

		oauth2.GET("/enabled", OAuth2EnabledAPI)

		oauth2.GET("/enabled/signup", OAuth2SignupEnabledAPI)

		oauth2.GET("/login/:type", OAuth2)

		oauth2.POST("/login/:type", OAuth2Api)

		oauth2.GET("/callback/:type", OAuth2Callback)

		oauth2.POST("/callback/:type", OAuth2CallbackAPI)

		needAuthOauth2.POST("/bind/:type", BindAPI)

		needAuthOauth2.POST("/unbind/:type", UnBindAPI)
	}
}
