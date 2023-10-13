package handlers

import (
	"fmt"
	"net/http"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/public"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/utils"
	rtmps "github.com/zijiren233/livelib/server"
)

func Init(e *gin.Engine, s *rtmps.Server, r *room.Rooms) {
	{
		s.SetParseChannelFunc(func(ReqAppName, ReqChannelName string, IsPublisher bool) (TrueAppName string, TrueChannel string, err error) {
			if IsPublisher {
				channelName, err := AuthRtmpPublish(ReqChannelName)
				if err != nil {
					log.Errorf("rtmp: publish auth to %s error: %v", ReqAppName, err)
					return "", "", err
				}
				if !r.HasRoom(ReqAppName) {
					log.Infof("rtmp: publish to %s/%s error: %s", ReqAppName, channelName, fmt.Sprintf("room %s not exist", ReqAppName))
					return "", "", fmt.Errorf("room %s not exist", ReqAppName)
				}
				log.Infof("rtmp: publish to success: %s/%s", ReqAppName, channelName)
				return ReqAppName, channelName, nil
			} else if !conf.Conf.Rtmp.RtmpPlayer {
				log.Infof("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
				return "", "", fmt.Errorf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
			}
			return ReqAppName, ReqChannelName, nil
		})
	}

	{
		web := e.Group("/web")

		web.Use(func(ctx *gin.Context) {
			if ctx.Request.URL.Path == "/web/" {
				ctx.Header("Cache-Control", "no-store")
			} else {
				ctx.Header("Cache-Control", "public, max-age=31536000")
			}
			ctx.Next()
		})

		web.StaticFS("", http.FS(public.Public))
	}

	{
		api := e.Group("/api")

		{
			public := api.Group("/public")

			public.GET("/settings", Settings)
		}

		{
			room := api.Group("/room")

			room.GET("/ws", NewWebSocketHandler(utils.NewWebSocketServer()))

			room.GET("/check", CheckRoom)

			room.GET("/user", CheckUser)

			room.GET("/list", RoomList)

			room.POST("/create", NewCreateRoomHandler(s))

			room.POST("/login", LoginRoom)

			room.POST("/delete", DeleteRoom)

			room.POST("/pwd", SetPassword)

			room.PUT("/admin", AddAdmin)

			room.DELETE("/admin", DelAdmin)
		}

		{
			movie := api.Group("/movie")

			movie.GET("/list", MovieList)

			movie.GET("/movies", Movies)

			movie.GET("/current", CurrentMovie)

			movie.POST("/current", ChangeCurrentMovie)

			movie.POST("/push", PushMovie)

			movie.POST("/edit", EditMovie)

			movie.POST("/swap", SwapMovie)

			movie.POST("/delete", DelMovie)

			movie.POST("/clear", ClearMovies)

			movie.HEAD("/proxy/:roomId/:pullKey", ProxyMovie)

			movie.GET("/proxy/:roomId/:pullKey", ProxyMovie)

			{
				live := movie.Group("/live")

				live.POST("/publishKey", NewPublishKey)

				live.GET("/*pullKey", JoinLive)
			}
		}

		{
			user := api.Group("/user")

			user.GET("/me", Me)

			user.POST("/pwd", SetUserPassword)
		}
	}

	e.NoRoute(func(c *gin.Context) {
		c.Redirect(http.StatusFound, "/web/")
	})
}
