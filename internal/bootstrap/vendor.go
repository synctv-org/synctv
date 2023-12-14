package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/vendor"
)

func InitVendorBackend(ctx context.Context) error {
	vb, err := db.GetAllVendorBackend()
	if err != nil {
		return err
	}
	b, err := vendor.NewBackends(ctx, vb)
	if err != nil {
		return err
	}
	vendor.StoreBackends(b)
	return nil
}
