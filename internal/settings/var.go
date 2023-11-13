package settings

import (
	"time"

	"github.com/synctv-org/synctv/internal/model"
)

var (
	DisableCreateRoom    = newBoolSetting("disable_create_room", false, model.SettingGroupRoom)
	RoomMustNeedPwd      = newBoolSetting("room_must_need_pwd", false, model.SettingGroupRoom)
	CreateRoomNeedReview = newBoolSetting("create_room_need_review", false, model.SettingGroupRoom)
	RoomTTL              = newInt64Setting("room_ttl", int64(time.Hour*48), model.SettingGroupRoom)
	UserMaxRoomCount     = newInt64Setting("user_max_room_count", 3, model.SettingGroupRoom)
)

var (
	DisableUserSignup = newBoolSetting("disable_user_signup", false, model.SettingGroupUser)
	SignupNeedReview  = newBoolSetting("signup_need_review", false, model.SettingGroupUser)
)

var (
	MovieProxy        = newBoolSetting("movie_proxy", true, model.SettingGroupProxy)
	LiveProxy         = newBoolSetting("live_proxy", true, model.SettingGroupProxy)
	AllowProxyToLocal = newBoolSetting("allow_proxy_to_local", false, model.SettingGroupProxy)
)

var (
	// can watch live streams through the RTMP protocol (without authentication, insecure).
	RtmpPlayer = newBoolSetting("rtmp_player", false, model.SettingGroupRtmp)
	// default use http header host
	CustomPublishHost = newStringSetting("custom_publish_host", "", model.SettingGroupRtmp)
	// disguise the .ts file as a .png file
	TsDisguisedAsPng = newBoolSetting("ts_disguised_as_png", true, model.SettingGroupRtmp)
)
