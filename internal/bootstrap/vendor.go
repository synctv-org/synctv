package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/vendor"
)

func InitVendorBackend(ctx context.Context) error {
	return vendor.Init(ctx)
}
