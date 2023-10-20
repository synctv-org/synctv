package middlewares

import "github.com/gin-gonic/gin"

func NewDistCacheControl(prefix string) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		if ctx.Request.URL.Path == prefix {
			ctx.Header("Cache-Control", "no-cache, max-age=300")
		} else {
			ctx.Header("Cache-Control", "public, max-age=31536000")
		}
		ctx.Next()
	}
}
