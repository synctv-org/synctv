package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/settings"
)

func InitSetting(ctx context.Context) error {
	return settings.Init()
}
