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
)

var (
	DisableUserSignup = newBoolSetting("disable_user_signup", false, model.SettingGroupUser)
	SignupNeedReview  = newBoolSetting("signup_need_review", false, model.SettingGroupUser)
)
