package handlers

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
)

func Settings(ctx *gin.Context) {
	ctx.JSON(200, NewApiDataResp(gin.H{
		"rtmp": gin.H{
			"enable":     conf.Conf.Rtmp.Enable,
			"rtmpPlayer": conf.Conf.Rtmp.RtmpPlayer,
			"hlsPlayer":  conf.Conf.Rtmp.HlsPlayer,
		},
		"proxy": gin.H{
			"movieProxy": conf.Conf.Proxy.MovieProxy,
			"liveProxy":  conf.Conf.Proxy.LiveProxy,
		},
	}))
}
