package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/vendor"
)

func InitVendor(ctx context.Context) error {
	return vendor.Init(&conf.Conf.Vendor)
}
