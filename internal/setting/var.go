package setting

import "github.com/synctv-org/synctv/internal/model"

var (
	DisableCreateRoom = newBoolSetting("disable_create_room", "0", model.SettingGroupRoom)
)
