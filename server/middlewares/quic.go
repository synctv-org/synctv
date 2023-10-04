package middlewares

import (
	"fmt"

	"github.com/gin-gonic/gin"
)

func NewQuic() gin.HandlerFunc {
	return func(c *gin.Context) {
		c.Header("Alt-Svc", fmt.Sprintf("h3=\":%s\"; ma=86400", c.Request.URL.Port()))
	}
}
