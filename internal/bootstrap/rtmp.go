package bootstrap

import (
	"context"
	"fmt"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	rtmps "github.com/zijiren233/livelib/server"
)

func InitRtmp(ctx context.Context) error {
	s := rtmps.NewRtmpServer(auth)
	rtmp.Init(s)
	return nil
}

func auth(ReqAppName, ReqChannelName string, IsPublisher bool) (*rtmps.Channel, error) {
	roomE, err := op.LoadOrInitRoomByID(ReqAppName)
	if err != nil {
		log.Errorf("rtmp: get room by id error: %v", err)
		return nil, err
	}
	room := roomE.Value()

	if err := validateRoom(room); err != nil {
		return nil, err
	}

	if IsPublisher {
		return handlePublisher(ReqAppName, ReqChannelName, room)
	}

	return handlePlayer(ReqAppName, ReqChannelName, room)
}

func validateRoom(room *op.Room) error {
	if room.IsBanned() {
		return fmt.Errorf("rtmp: room %s is banned", room.ID)
	}
	if room.IsPending() {
		return fmt.Errorf("rtmp: room %s is pending, need admin approval", room.ID)
	}
	return nil
}

func handlePublisher(ReqAppName, ReqChannelName string, room *op.Room) (*rtmps.Channel, error) {
	channelName, err := rtmp.AuthRtmpPublish(ReqChannelName)
	if err != nil {
		log.Errorf("rtmp: publish auth to %s error: %v", ReqAppName, err)
		return nil, err
	}
	log.Infof("rtmp: publisher login success: %s/%s", ReqAppName, channelName)
	return room.GetChannel(channelName)
}

func handlePlayer(ReqAppName, ReqChannelName string, room *op.Room) (*rtmps.Channel, error) {
	if !settings.RtmpPlayer.Get() {
		err := fmt.Errorf("rtmp player is not enabled")
		log.Warnf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, err)
		return nil, err
	}
	return room.GetChannel(ReqChannelName)
}
