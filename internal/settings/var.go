package settings

import (
	"errors"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

func BeforeSetBoolFunc(s BoolSetting, v bool) error {
	return db.UpdateSettingItemValue(s.Name(), s.Stringify(v))
}

func BeforeSetStringFunc(s StringSetting, v string) error {
	return db.UpdateSettingItemValue(s.Name(), s.Stringify(v))
}

func BeforeSetInt64Func(s Int64Setting, v int64) error {
	return db.UpdateSettingItemValue(s.Name(), s.Stringify(v))
}

func BeforeSetFloat64Func(s Float64Setting, v float64) error {
	return db.UpdateSettingItemValue(s.Name(), s.Stringify(v))
}

var (
	DisableCreateRoom    = NewBoolSetting("disable_create_room", false, model.SettingGroupRoom, WithBeforeSetBool(BeforeSetBoolFunc))
	RoomMustNeedPwd      = NewBoolSetting("room_must_need_pwd", false, model.SettingGroupRoom, WithBeforeSetBool(BeforeSetBoolFunc))
	CreateRoomNeedReview = NewBoolSetting("create_room_need_review", false, model.SettingGroupRoom, WithBeforeSetBool(BeforeSetBoolFunc))
	RoomTTL              = NewInt64Setting("room_ttl", int64(time.Hour*48), model.SettingGroupRoom, WithBeforeSetInt64(BeforeSetInt64Func))
	UserMaxRoomCount     = NewInt64Setting("user_max_room_count", 3, model.SettingGroupRoom, WithBeforeSetInt64(BeforeSetInt64Func))
)

var (
	DisableUserSignup = NewBoolSetting("disable_user_signup", false, model.SettingGroupUser, WithBeforeSetBool(BeforeSetBoolFunc))
	SignupNeedReview  = NewBoolSetting("signup_need_review", false, model.SettingGroupUser, WithBeforeSetBool(BeforeSetBoolFunc))
)

var (
	MovieProxy        = NewBoolSetting("movie_proxy", true, model.SettingGroupProxy, WithBeforeSetBool(BeforeSetBoolFunc))
	LiveProxy         = NewBoolSetting("live_proxy", true, model.SettingGroupProxy, WithBeforeSetBool(BeforeSetBoolFunc))
	AllowProxyToLocal = NewBoolSetting("allow_proxy_to_local", false, model.SettingGroupProxy, WithBeforeSetBool(BeforeSetBoolFunc))
)

var (
	// can watch live streams through the RTMP protocol (without authentication, insecure).
	RtmpPlayer = NewBoolSetting("rtmp_player", false, model.SettingGroupRtmp, WithBeforeSetBool(BeforeSetBoolFunc))
	// default use http header host
	CustomPublishHost = NewStringSetting("custom_publish_host", "", model.SettingGroupRtmp, WithBeforeSetString(BeforeSetStringFunc))
	// disguise the .ts file as a .png file
	TsDisguisedAsPng = NewBoolSetting("ts_disguised_as_png", true, model.SettingGroupRtmp, WithBeforeSetBool(BeforeSetBoolFunc))
)

var (
	DatabaseVersion = NewStringSetting("database_version", "0.0.1", model.SettingGroupDatabase, WithBeforeSetString(func(ss StringSetting, s string) error {
		return errors.New("not support change database version")
	}))
)
