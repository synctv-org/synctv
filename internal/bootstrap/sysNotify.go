package bootstrap

import sysnotify "github.com/synctv-org/synctv/utils/sysNotify"

var (
	SysNotify *sysnotify.SysNotify
)

func InitSysNotify() {
	SysNotify = sysnotify.New()
}
