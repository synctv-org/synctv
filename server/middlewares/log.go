package middlewares

import (
	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
)

func NewLog(l *logrus.Logger) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		ctx.Set("log", &logrus.Entry{
			Logger: l,
		})
	}
}
