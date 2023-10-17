package server

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/server/handlers"
	"github.com/synctv-org/synctv/server/middlewares"
)

func Init(e *gin.Engine) {
	middlewares.Init(e)
	handlers.Init(e)
}

func NewAndInit() (e *gin.Engine) {
	e = gin.New()
	Init(e)
	return
}
