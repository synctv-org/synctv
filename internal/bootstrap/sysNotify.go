package bootstrap

import (
	"context"

	sysnotify "github.com/synctv-org/synctv/utils/sysNotify"
)

var (
	SysNotify sysnotify.SysNotify
)

func InitSysNotify(ctx context.Context) error {
	SysNotify.Init()
	return nil
}
