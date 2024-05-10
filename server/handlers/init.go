package handlers

import (
	"errors"
	"net/url"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/handlers/vendors"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendorAlist"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendorBilibili"
	"github.com/synctv-org/synctv/server/handlers/vendors/vendorEmby"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/utils"
)

var (
	HOST = settings.NewStringSetting(
		"host",
		"",
		model.SettingGroupServer,
		settings.WithValidatorString(func(s string) error {
			if s == "" {
				return nil
			}
			if !strings.HasPrefix(s, "http://") && !strings.HasPrefix(s, "https://") {
				return errors.New("host must start with http:// or https://")
			}
			_, err := url.Parse(s)
			return err
		}),
	)
)

func Init(e *gin.Engine) {
	api := e.Group("/api")

	needAuthUserApi := api.Group("", middlewares.AuthUserMiddleware)

	needAuthRoomApi := api.Group("", middlewares.AuthRoomMiddleware)

	needAuthRoomWithoutGuestApi := api.Group("", middlewares.AuthRoomWithoutGuestMiddleware)

	{
		public := api.Group("/public")

		public.GET("/settings", Settings)
	}

	{
		admin := api.Group("/admin")
		root := api.Group("/admin")
		admin.Use(middlewares.AuthAdminMiddleware)
		root.Use(middlewares.AuthRootMiddleware)

		initAdmin(admin, root)
	}

	{
		room := api.Group("/room")
		needAuthUser := needAuthUserApi.Group("/room")
		needAuthRoom := needAuthRoomApi.Group("/room")
		needAuthRoomWithoutGuest := needAuthRoomWithoutGuestApi.Group("/room")

		initRoom(room, needAuthUser, needAuthRoom, needAuthRoomWithoutGuest)
	}

	{
		movie := api.Group("/movie")
		needAuthMovie := needAuthRoomApi.Group("/movie")

		initMovie(movie, needAuthMovie)
	}

	{
		user := api.Group("/user")
		needAuthUser := needAuthUserApi.Group("/user")

		initUser(user, needAuthUser)
	}

	{
		vendor := needAuthUserApi.Group("/vendor")

		initVendor(vendor)
	}
}

func initAdmin(admin *gin.RouterGroup, root *gin.RouterGroup) {
	{
		admin.GET("/settings", AdminSettings)

		admin.GET("/settings/:group", AdminSettings)

		admin.POST("/settings", EditAdminSettings)

		admin.POST("/email/test", SendTestEmail)

		admin.GET("/vendors", AdminGetVendorBackends)

		admin.POST("/vendors/add", AdminAddVendorBackend)

		admin.POST("/vendors/update", AdminUpdateVendorBackends)

		admin.POST("/vendors/delete", AdminDeleteVendorBackends)

		admin.POST("/vendors/reconnect", AdminReconnectVendorBackends)

		admin.POST("/vendors/enable", AdminEnableVendorBackends)

		admin.POST("/vendors/disable", AdminDisableVendorBackends)

		{
			user := admin.Group("/user")

			user.POST("/add", AddUser)

			user.POST("/delete", DeleteUser)

			user.POST("/password", AdminUserPassword)

			user.POST("/username", AdminUsername)

			// 查找用户
			user.GET("/list", Users)

			user.POST("/approve", ApprovePendingUser)

			user.POST("/ban", BanUser)

			user.POST("/unban", UnBanUser)

			// 查找某个用户的房间
			user.GET("/rooms", GetUserRooms)
		}

		{
			room := admin.Group("/room")

			room.POST("/password", AdminRoomPassword)

			// 查找房间
			room.GET("/list", Rooms)

			room.POST("/approve", ApprovePendingRoom)

			room.POST("/ban", BanRoom)

			room.POST("/unban", UnBanRoom)

			room.POST("/delete", AdminDeleteRoom)

			room.GET("/members", AdminGetRoomMembers)
		}
	}

	{
		root.POST("/admin/add", AddAdmin)

		root.POST("/admin/delete", DeleteAdmin)
	}
}

func initRoom(room *gin.RouterGroup, needAuthUser *gin.RouterGroup, needAuthRoom *gin.RouterGroup, needAuthWithoutGuestRoom *gin.RouterGroup) {
	room.GET("/ws", NewWebSocketHandler(utils.NewWebSocketServer()))

	room.GET("/check", CheckRoom)

	room.GET("/hot", RoomHotList)

	room.GET("/list", RoomList)

	room.POST("/guest", GuestJoinRoom)

	needAuthUser.POST("/create", CreateRoom)

	needAuthUser.POST("/login", LoginRoom)

	needAuthRoom.GET("/me", RoomMe)

	needAuthWithoutGuestRoom.GET("/settings", RoomPiblicSettings)

	needAuthWithoutGuestRoom.GET("/members", RoomMembers)

	{
		needAuthRoomAdmin := needAuthRoom.Group("/admin", middlewares.AuthRoomAdminMiddleware)
		needAuthRoomCreator := needAuthRoom.Group("/admin", middlewares.AuthRoomCreatorMiddleware)

		needAuthRoomAdmin.GET("/settings", RoomSetting)

		needAuthRoomAdmin.POST("/settings", SetRoomSetting)

		needAuthRoomAdmin.POST("/delete", DeleteRoom)

		needAuthRoomAdmin.POST("/pwd", SetRoomPassword)

		needAuthRoomAdmin.GET("/members", RoomAdminMembers)

		needAuthRoomAdmin.POST("/members/approve", RoomAdminApproveMember)

		needAuthRoomAdmin.POST("/members/ban", RoomAdminBanMember)

		needAuthRoomAdmin.POST("/members/unban", RoomAdminUnbanMember)

		needAuthRoomCreator.POST("/members/member", RoomSetMember)

		needAuthRoomCreator.POST("/members/member/permissions", RoomSetMemberPermissions)

		needAuthRoomCreator.POST("/members/admin", RoomSetAdmin)

		needAuthRoomCreator.POST("/members/admin/permissions", RoomSetAdminPermissions)
	}
}

func initMovie(movie *gin.RouterGroup, needAuthMovie *gin.RouterGroup) {
	// needAuthMovie.GET("/list", MovieList)

	needAuthMovie.GET("/current", CurrentMovie)

	needAuthMovie.GET("/movies", Movies)

	needAuthMovie.POST("/current", ChangeCurrentMovie)

	needAuthMovie.POST("/push", PushMovie)

	needAuthMovie.POST("/pushs", PushMovies)

	needAuthMovie.POST("/edit", EditMovie)

	needAuthMovie.POST("/swap", SwapMovie)

	needAuthMovie.POST("/delete", DelMovie)

	needAuthMovie.POST("/clear", ClearMovies)

	needAuthMovie.HEAD("/proxy/:roomId/:movieId", ProxyMovie)

	needAuthMovie.GET("/proxy/:roomId/:movieId", ProxyMovie)

	{
		needAuthLive := needAuthMovie.Group("/live")

		needAuthLive.POST("/publishKey", NewPublishKey)

		// needAuthLive.GET("/join/:movieId", JoinLive)

		needAuthLive.GET("/flv/:movieId", JoinFlvLive)

		needAuthLive.GET("/hls/list/:movieId", JoinHlsLive)

		needAuthLive.GET("/hls/data/:roomId/:movieId/:dataId", ServeHlsLive)
	}
}

func initUser(user *gin.RouterGroup, needAuthUser *gin.RouterGroup) {
	user.POST("/login", LoginUser)

	user.GET("/signup/email/captcha", GetUserSignupEmailStep1Captcha)

	user.POST("/signup/email/captcha", SendUserSignupEmailCaptcha)

	user.POST("/signup/email", UserSignupEmail)

	user.GET("/retrieve/email/captcha", GetUserRetrievePasswordEmailStep1Captcha)

	user.POST("/retrieve/email/captcha", SendUserRetrievePasswordEmailCaptcha)

	user.POST("/retrieve/email", UserRetrievePasswordEmail)

	needAuthUser.POST("/logout", LogoutUser)

	needAuthUser.GET("/me", Me)

	needAuthUser.GET("/rooms", UserRooms)

	needAuthUser.POST("/username", SetUsername)

	needAuthUser.POST("/password", SetUserPassword)

	needAuthUser.GET("/providers", UserBindProviders)

	needAuthUser.GET("/bind/email/captcha", GetUserBindEmailStep1Captcha)

	needAuthUser.POST("/bind/email/captcha", SendUserBindEmailCaptcha)

	needAuthUser.POST("/bind/email", UserBindEmail)

	needAuthUser.POST("/unbind/email", UserUnbindEmail)

	{
		room := needAuthUser.Group("/room")

		room.POST("/delete", UserDeleteRoom)
	}
}

func initVendor(vendor *gin.RouterGroup) {
	vendor.GET("/backends/:vendor", vendors.Backends)

	{
		bilibili := vendor.Group("/bilibili")

		login := bilibili.Group("/login")

		login.GET("/qr", vendorBilibili.NewQRCode)

		login.POST("/qr", vendorBilibili.LoginWithQR)

		login.GET("/captcha", vendorBilibili.NewCaptcha)

		login.POST("/sms/send", vendorBilibili.NewSMS)

		login.POST("/sms/login", vendorBilibili.LoginWithSMS)

		bilibili.POST("/parse", vendorBilibili.Parse)

		bilibili.GET("/me", vendorBilibili.Me)

		bilibili.POST("/logout", vendorBilibili.Logout)
	}

	{
		alist := vendor.Group("/alist")

		alist.POST("/login", vendorAlist.Login)

		alist.POST("/logout", vendorAlist.Logout)

		alist.POST("/list", vendorAlist.List)

		alist.GET("/me", vendorAlist.Me)

		alist.GET("/binds", vendorAlist.Binds)
	}

	{
		emby := vendor.Group("/emby")

		emby.POST("/login", vendorEmby.Login)

		emby.POST("/logout", vendorEmby.Logout)

		emby.POST("/list", vendorEmby.List)

		emby.GET("/me", vendorEmby.Me)

		emby.GET("/binds", vendorEmby.Binds)
	}
}
