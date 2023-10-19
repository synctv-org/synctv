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
	s := rtmps.NewRtmpServer(auth)
	rtmp.Init(s)

	return nil
}

func auth(ReqAppName, ReqChannelName string, IsPublisher bool) (*rtmps.Channel, error) {
	if IsPublisher {
		channelName, err := rtmp.AuthRtmpPublish(ReqChannelName)
		if err != nil {
			log.Errorf("rtmp: publish auth to %s error: %v", ReqAppName, err)
			return nil, err
		}
		log.Infof("rtmp: publisher login success: %s/%s", ReqAppName, channelName)
		id, err := strconv.Atoi(ReqAppName)
		if err != nil {
			log.Errorf("rtmp: parse channel name to id error: %v", err)
			return nil, err
		}
		r, err := op.GetRoomByID(uint(id))
		if err != nil {
			log.Errorf("rtmp: get room by id error: %v", err)
			return nil, err
		}
		return r.GetChannel(channelName)
	}

	if !conf.Conf.Rtmp.RtmpPlayer {
		log.Warnf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
		return nil, fmt.Errorf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
	}
	id, err := strconv.Atoi(ReqAppName)
	if err != nil {
		log.Errorf("rtmp: parse channel name to id error: %v", err)
		return nil, err
	}
	r, err := op.GetRoomByID(uint(id))
	if err != nil {
		log.Errorf("rtmp: get room by id error: %v", err)
		return nil, err
	}
	return r.GetChannel(ReqChannelName)
}
