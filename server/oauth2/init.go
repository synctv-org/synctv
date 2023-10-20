package auth

import "github.com/gin-gonic/gin"

func Init(e *gin.Engine) {
	{
		auth := e.Group("/oauth2")

		auth.GET("/login/:type", OAuth2)

		auth.GET("/callback/:type", OAuth2Callback)
	}
}
