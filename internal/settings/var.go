package settings

import (
	"errors"
	"time"

	"github.com/synctv-org/synctv/internal/model"
)

var (
	DisableCreateRoom    = NewBoolSetting("disable_create_room", false, model.SettingGroupRoom)
	RoomMustNeedPwd      = NewBoolSetting("room_must_need_pwd", false, model.SettingGroupRoom)
	CreateRoomNeedReview = NewBoolSetting("create_room_need_review", false, model.SettingGroupRoom)
	RoomTTL              = NewInt64Setting("room_ttl", int64(time.Hour*48), model.SettingGroupRoom)
)

var (
	DisableUserSignup = NewBoolSetting("disable_user_signup", false, model.SettingGroupUser)
	SignupNeedReview  = NewBoolSetting("signup_need_review", false, model.SettingGroupUser)
	UserMaxRoomCount  = NewInt64Setting("user_max_room_count", 3, model.SettingGroupUser)
)

var (
	MovieProxy        = NewBoolSetting("movie_proxy", true, model.SettingGroupProxy)
	LiveProxy         = NewBoolSetting("live_proxy", true, model.SettingGroupProxy)
	AllowProxyToLocal = NewBoolSetting("allow_proxy_to_local", false, model.SettingGroupProxy)
)

var (
	// can watch live streams through the RTMP protocol (without authentication, insecure).
	RtmpPlayer = NewBoolSetting("rtmp_player", false, model.SettingGroupRtmp)
	// default use http header host
	CustomPublishHost = NewStringSetting("custom_publish_host", "", model.SettingGroupRtmp)
	// disguise the .ts file as a .png file
	TsDisguisedAsPng = NewBoolSetting("ts_disguised_as_png", true, model.SettingGroupRtmp)
)

var (
	DatabaseVersion = NewStringSetting("database_version", "0.0.1", model.SettingGroupDatabase, WithBeforeSetString(func(ss StringSetting, s string) (string, error) {
		return "", errors.New("not support change database version")
	}))
)
