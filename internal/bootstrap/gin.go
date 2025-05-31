package bootstrap

import (
	"context"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/utils"
)

func InitGinMode(_ context.Context) error {
	if flags.Global.Dev {
		gin.SetMode(gin.DebugMode)
	} else {
		gin.SetMode(gin.ReleaseMode)
	}
	if utils.ForceColor() {
		gin.ForceConsoleColor()
	} else {
		gin.DisableConsoleColor()
	}

	return nil
}
