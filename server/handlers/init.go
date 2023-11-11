package handlers

import (
	"github.com/gin-gonic/gin"
	Vbilibili "github.com/synctv-org/synctv/server/handlers/vendors/bilibili"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/utils"
)

func Init(e *gin.Engine) {
	{
		api := e.Group("/api")

		needAuthUserApi := api.Group("")
		needAuthUserApi.Use(middlewares.AuthUserMiddleware)

		needAuthRoomApi := api.Group("")
		needAuthRoomApi.Use(middlewares.AuthRoomMiddleware)

		{
			public := api.Group("/public")

			public.GET("/settings", Settings)
		}

		{
			admin := api.Group("/admin")
			root := api.Group("/admin")
			admin.Use(middlewares.AuthAdminMiddleware)
			root.Use(middlewares.AuthRootMiddleware)

			{
				admin.GET("/settings/:group", AdminSettings)

				admin.POST("/settings", EditAdminSettings)

				admin.GET("/users", Users)

				admin.GET("/rooms", Rooms)

				admin.POST("/approve/user", ApprovePendingUser)

				admin.POST("/approve/room", ApprovePendingRoom)

				admin.POST("/ban/user", BanUser)

				admin.POST("/ban/room", BanRoom)
			}

			{
				root.POST("/admin/add", AddAdmin)

				root.POST("/admin/delete", DeleteAdmin)
			}
		}

		{
			room := api.Group("/room")
			needAuthRoom := needAuthRoomApi.Group("/room")
			needAuthUser := needAuthUserApi.Group("/room")

			room.GET("/ws", NewWebSocketHandler(utils.NewWebSocketServer()))

			room.GET("/check", CheckRoom)

			room.GET("/hot", RoomHotList)

			room.GET("/list", RoomList)

			needAuthUser.POST("/create", CreateRoom)

			needAuthUser.POST("/login", LoginRoom)

			needAuthRoom.POST("/delete", DeleteRoom)

			needAuthRoom.POST("/pwd", SetRoomPassword)

			needAuthRoom.GET("/settings", RoomSetting)

			needAuthRoom.POST("/settings", SetRoomSetting)

			// needAuthRoom.GET("/users", RoomUsers)
		}

		{
			movie := api.Group("/movie")
			needAuthMovie := needAuthRoomApi.Group("/movie")

			needAuthMovie.GET("/list", MovieList)

			needAuthMovie.GET("/current", CurrentMovie)

			needAuthMovie.GET("/movies", Movies)

			needAuthMovie.POST("/current", ChangeCurrentMovie)

			needAuthMovie.POST("/push", PushMovie)

			needAuthMovie.POST("/pushs", PushMovies)

			needAuthMovie.POST("/edit", EditMovie)

			needAuthMovie.POST("/swap", SwapMovie)

			needAuthMovie.POST("/delete", DelMovie)

			needAuthMovie.POST("/clear", ClearMovies)

			movie.HEAD("/proxy/:roomId/:movieId", ProxyMovie)

			movie.GET("/proxy/:roomId/:movieId", ProxyMovie)

			{
				live := needAuthMovie.Group("/live")

				live.POST("/publishKey", NewPublishKey)

				live.GET("/*movieId", JoinLive)
			}
		}

		{
			// user := api.Group("/user")
			needAuthUser := needAuthUserApi.Group("/user")

			needAuthUser.POST("/logout", LogoutUser)

			needAuthUser.GET("/me", Me)

			needAuthUser.GET("/rooms", UserRooms)

			needAuthUser.POST("/username", SetUsername)
		}

		{
			vendor := needAuthUserApi.Group("/vendor")

			{
				bilibili := vendor.Group("/bilibili")

				login := bilibili.Group("/login")

				login.GET("/qr", Vbilibili.NewQRCode)

				login.POST("/qr", Vbilibili.LoginWithQR)

				login.GET("/captcha", Vbilibili.NewCaptcha)

				login.POST("/sms/send", Vbilibili.NewSMS)

				login.POST("/sms/login", Vbilibili.LoginWithSMS)

				bilibili.POST("/parse", Vbilibili.Parse)

				bilibili.GET("/vendors", Vbilibili.Vendors)

				bilibili.GET("/me", Vbilibili.Me)

				bilibili.POST("/logout", Vbilibili.Logout)
			}
		}
	}
}
