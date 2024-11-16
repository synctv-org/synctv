package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/op"
)

func InitOp(ctx context.Context) error {
	return op.Init(4096)
}
