package server

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/server/handlers"
	"github.com/synctv-org/synctv/server/middlewares"
	rtmps "github.com/zijiren233/livelib/server"
)

func Init(e *gin.Engine, s *rtmps.Server) {
	r := room.NewRooms()
	middlewares.Init(e, r)
	handlers.Init(e, s, r)
}

func NewAndInit() (e *gin.Engine, s *rtmps.Server) {
	e = gin.New()
	s = rtmps.NewRtmpServer(rtmps.WithInitHlsPlayer(conf.Conf.Rtmp.HlsPlayer))
	Init(e, s)
	return
}
