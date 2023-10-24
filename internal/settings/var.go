package settings

import "github.com/synctv-org/synctv/internal/model"

var (
	DisableCreateRoom = newBoolSetting("disable_create_room", false, model.SettingGroupRoom)
)
