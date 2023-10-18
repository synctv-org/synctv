package bootstrap

import (
	"context"
	"fmt"
	"strconv"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	rtmps "github.com/zijiren233/livelib/server"
)

func InitRtmp(ctx context.Context) error {
	s := rtmps.NewRtmpServer(rtmps.WithInitHlsPlayer(true))
	rtmp.Init(s)

	s.SetParseChannelFunc(func(ReqAppName, ReqChannelName string, IsPublisher bool) (TrueAppName string, TrueChannel string, err error) {
		if IsPublisher {
			channelName, err := rtmp.AuthRtmpPublish(ReqChannelName)
			if err != nil {
				log.Errorf("rtmp: publish auth to %s error: %v", ReqAppName, err)
				return "", "", err
			}
			log.Infof("rtmp: publisher login success: %s/%s", ReqAppName, channelName)
			id, err := strconv.Atoi(ReqAppName)
			if err != nil {
				log.Errorf("rtmp: parse channel name to id error: %v", err)
				return "", "", err
			}
			r, err := op.GetRoomByID(uint(id))
			if err != nil {
				log.Errorf("rtmp: get room by id error: %v", err)
				return "", "", err
			}
			err = r.LazyInit()
			if err != nil {
				log.Errorf("rtmp: lazy init room error: %v", err)
				return "", "", err
			}
			return ReqAppName, channelName, nil
		} else if !conf.Conf.Rtmp.RtmpPlayer {
			log.Warnf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
			return "", "", fmt.Errorf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
		}
		return ReqAppName, ReqChannelName, nil
	})
	return nil
}
