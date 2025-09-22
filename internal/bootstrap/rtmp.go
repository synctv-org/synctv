package bootstrap

import (
	"context"
	"errors"
	"fmt"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	rtmps "github.com/zijiren233/livelib/server"
)

func InitRtmp(_ context.Context) error {
	s := rtmps.NewRtmpServer(auth)
	rtmp.Init(s)
	return nil
}

func auth(reqAppName, reqChannelName string, isPublisher bool) (*rtmps.Channel, error) {
	roomE, err := op.LoadOrInitRoomByID(reqAppName)
	if err != nil {
		log.Errorf("rtmp: get room by id error: %v", err)
		return nil, err
	}

	room := roomE.Value()

	if err := validateRoom(room); err != nil {
		return nil, err
	}

	if isPublisher {
		return handlePublisher(reqAppName, reqChannelName, room)
	}

	return handlePlayer(reqAppName, reqChannelName, room)
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

func handlePublisher(reqAppName, reqChannelName string, room *op.Room) (*rtmps.Channel, error) {
	channelName, err := rtmp.AuthRtmpPublish(reqChannelName)
	if err != nil {
		log.Errorf("rtmp: publish auth to %s error: %v", reqAppName, err)
		return nil, err
	}

	log.Infof("rtmp: publisher login success: %s/%s", reqAppName, channelName)

	return room.GetChannel(channelName)
}

func handlePlayer(reqAppName, reqChannelName string, room *op.Room) (*rtmps.Channel, error) {
	if !settings.RtmpPlayer.Get() {
		err := errors.New("rtmp player is not enabled")
		log.Warnf("rtmp: dial to %s/%s error: %s", reqAppName, reqChannelName, err)
		return nil, err
	}

	return room.GetChannel(reqChannelName)
}
