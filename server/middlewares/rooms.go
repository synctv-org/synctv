package middlewares

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/room"
)

func NewRooms(r *room.Rooms) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		ctx.Set("rooms", r)
		ctx.Next()
	}
}
