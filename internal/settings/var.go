package settings

import (
	"errors"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var (
	DisableCreateRoom    = NewBoolSetting("disable_create_room", false, model.SettingGroupRoom)
	RoomMustNeedPwd      BoolSetting
	RoomMustNoNeedPwd    BoolSetting
	CreateRoomNeedReview = NewBoolSetting("create_room_need_review", false, model.SettingGroupRoom)
	// default 48 hours
	RoomTTL = NewInt64Setting("room_ttl", 48, model.SettingGroupRoom, WithBeforeSetInt64(func(is Int64Setting, i int64) (int64, error) {
		if i < 1 {
			return 0, errors.New("room ttl must be greater than 0")
		}
		return i, nil
	}))
)

func init() {
	RoomMustNeedPwd = NewBoolSetting(
		"room_must_need_pwd",
		false,
		model.SettingGroupRoom,
		WithBeforeSetBool(func(bs BoolSetting, b bool) (bool, error) {
			if b && RoomMustNoNeedPwd.Get() {
				return false, errors.New("room_must_need_pwd and room_must_no_need_pwd can't be true at the same time")
			}
			return b, nil
		}),
	)
	RoomMustNoNeedPwd = NewBoolSetting(
		"room_must_no_need_pwd",
		false,
		model.SettingGroupRoom,
		WithBeforeSetBool(func(bs BoolSetting, b bool) (bool, error) {
			if b && RoomMustNeedPwd.Get() {
				return false, errors.New("room_must_need_pwd and room_must_no_need_pwd can't be true at the same time")
			}
			return b, nil
		}),
	)
}

var (
	DisableUserSignup        = NewBoolSetting("disable_user_signup", false, model.SettingGroupUser)
	SignupNeedReview         = NewBoolSetting("signup_need_review", false, model.SettingGroupUser)
	EnablePasswordSignup     = NewBoolSetting("enable_password_signup", false, model.SettingGroupUser)
	PasswordSignupNeedReview = NewBoolSetting("password_signup_need_review", false, model.SettingGroupUser)
	UserMaxRoomCount         = NewInt64Setting("user_max_room_count", 3, model.SettingGroupUser)
	EnableGuest              = NewBoolSetting("enable_guest", true, model.SettingGroupUser)
)

var (
	MovieProxy        = NewBoolSetting("movie_proxy", true, model.SettingGroupProxy)
	LiveProxy         = NewBoolSetting("live_proxy", true, model.SettingGroupProxy)
	AllowProxyToLocal = NewBoolSetting("allow_proxy_to_local", false, model.SettingGroupProxy)
	ProxyCacheEnable  = NewBoolSetting("proxy_cache_enable", false, model.SettingGroupProxy)
)

var (
	// can watch live streams through the RTMP protocol (without authentication, insecure).
	RtmpPlayer = NewBoolSetting("rtmp_player", false, model.SettingGroupRtmp)
	// default use http header host
	CustomPublishHost = NewStringSetting("custom_publish_host", "", model.SettingGroupRtmp)
	// disguise the .ts file as a .png file
	TsDisguisedAsPng = NewBoolSetting("ts_disguised_as_png", true, model.SettingGroupRtmp)
)

var DatabaseVersion = NewStringSetting("database_version", db.CurrentVersion, model.SettingGroupDatabase, WithBeforeSetString(func(ss StringSetting, s string) (string, error) {
	return "", errors.New("not support change database version")
}))
