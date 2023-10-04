package cmd

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/cmd/flags"
)

func InitGinMode() error {
	if flags.Dev {
		gin.SetMode(gin.DebugMode)
	} else {
		gin.SetMode(gin.ReleaseMode)
	}
	gin.ForceConsoleColor()

	return nil
}
