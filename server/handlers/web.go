package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/public"
)

func WebServer(e gin.IRoutes) {
	e.StaticFS("", http.FS(public.Public))
}
