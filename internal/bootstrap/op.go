package bootstrap

import (
	"context"
	"time"

	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/op"
)

func InitOp(ctx context.Context) error {
	d, err := time.ParseDuration(conf.Conf.Room.TTL)
	if err != nil {
		return err
	}
	op.Init(4096, d)
	return nil
}
