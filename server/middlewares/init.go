package middlewares

import (
	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/room"
)

func Init(e *gin.Engine, r *room.Rooms) {
	w := log.StandardLogger().Writer()
	e.
		Use(gin.LoggerWithWriter(w), gin.RecoveryWithWriter(w)).
		Use(NewCors()).
		Use(NewRooms(r))
	if conf.Conf.Server.Quic && conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "" {
		e.Use(NewQuic())
	}
}
