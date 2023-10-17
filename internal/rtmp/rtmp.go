package rtmp

import (
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/golang-jwt/jwt/v5"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
)

var s *rtmps.Server

type RtmpClaims struct {
	PullKey string `json:"p"`
	jwt.RegisteredClaims
}

func AuthRtmpPublish(Authorization string) (channelName string, err error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &RtmpClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil {
		return "", errors.New("auth failed")
	}
	claims, ok := t.Claims.(*RtmpClaims)
	if !ok {
		return "", errors.New("auth failed")
	}
	return claims.PullKey, nil
}

func NewRtmpAuthorization(channelName string) (string, error) {
	claims := &RtmpClaims{
		PullKey: channelName,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func Init(rs *rtmps.Server) {
	s = rs

	rs.SetParseChannelFunc(func(ReqAppName, ReqChannelName string, IsPublisher bool) (TrueAppName string, TrueChannel string, err error) {
		if IsPublisher {
			channelName, err := AuthRtmpPublish(ReqChannelName)
			if err != nil {
				log.Errorf("rtmp: publish auth to %s error: %v", ReqAppName, err)
				return "", "", err
			}
			log.Infof("rtmp: publisher login success: %s/%s", ReqAppName, channelName)
			return ReqAppName, channelName, nil
		} else if !conf.Conf.Rtmp.RtmpPlayer {
			log.Warnf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
			return "", "", fmt.Errorf("rtmp: dial to %s/%s error: %s", ReqAppName, ReqChannelName, "rtmp player is not enabled")
		}
		return ReqAppName, ReqChannelName, nil
	})
}

func RtmpServer() *rtmps.Server {
	return s
}
