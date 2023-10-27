package settings

import "github.com/synctv-org/synctv/internal/model"

var (
	DisableCreateRoom = newBoolSetting("disable_create_room", false, model.SettingGroupRoom)
)

var (
	DisableUserSignup = newBoolSetting("disable_user_signup", false, model.SettingGroupUser)
	SignupNeedReview  = newBoolSetting("signup_need_review", false, model.SettingGroupUser)
)
