package handlers

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/server/model"
)

func Settings(ctx *gin.Context) {
	ctx.JSON(200, model.NewApiDataResp(gin.H{
		"rtmp": gin.H{
			"enable":     conf.Conf.Rtmp.Enable,
			"rtmpPlayer": conf.Conf.Rtmp.RtmpPlayer,
		},
		"proxy": gin.H{
			"movieProxy": conf.Conf.Proxy.MovieProxy,
			"liveProxy":  conf.Conf.Proxy.LiveProxy,
		},
		"room": gin.H{
			"mustPassword": conf.Conf.Room.MustPassword,
		},
	}))
}
