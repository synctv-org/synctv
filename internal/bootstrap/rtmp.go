package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/rtmp"
	rtmps "github.com/zijiren233/livelib/server"
)

func InitRtmp(ctx context.Context) error {
	s := rtmps.NewRtmpServer(rtmps.WithInitHlsPlayer(conf.Conf.Rtmp.HlsPlayer))
	rtmp.Init(s)
	return nil
}
