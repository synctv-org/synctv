package bootstrap

import (
	"context"

	sysnotify "github.com/synctv-org/synctv/internal/sysnotify"
)

func InitSysNotify(_ context.Context) error {
	sysnotify.Init()
	return nil
}
