package middlewares

import (
	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
)

func Init(e *gin.Engine) {
	w := log.StandardLogger().Writer()
	e.
		Use(gin.LoggerWithWriter(w), gin.RecoveryWithWriter(w)).
		Use(NewCors())
	if conf.Conf.Server.Quic && conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "" {
		e.Use(NewQuic())
	}
}
