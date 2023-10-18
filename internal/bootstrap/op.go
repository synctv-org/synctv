package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/op"
)

func InitOp(ctx context.Context) error {
	op.Init(4096)
	return nil
}
