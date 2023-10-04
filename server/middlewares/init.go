package middlewares

import (
	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
)

func Init(e *gin.Engine) {
	e.
		Use(gin.LoggerWithWriter(log.StandardLogger().Out), gin.RecoveryWithWriter(log.StandardLogger().Out)).
		Use(NewCors())
	if conf.Conf.Server.Quic {
		e.Use(NewQuic())
	}
}
