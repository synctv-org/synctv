package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/setting"
)

func InitSetting(ctx context.Context) error {
	return setting.Init()
}
