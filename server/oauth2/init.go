package auth

import "github.com/gin-gonic/gin"

func Init(e *gin.Engine) {
	{
		auth := e.Group("/oauth2")

		auth.GET("/enabled", OAuth2EnabledApi)

		auth.GET("/login/:type", OAuth2)

		auth.POST("/login/:type", OAuth2Api)

		auth.GET("/callback/:type", OAuth2Callback)

		auth.POST("/callback/:type", OAuth2CallbackApi)
	}
}
