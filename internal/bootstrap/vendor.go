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
	bc, err := vendor.NewBackendConns(ctx, vb)
	if err != nil {
		return err
	}
	return vendor.StoreConns(bc)
}
