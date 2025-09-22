package rtmp

import (
	"errors"
	"strings"
	"time"

	"github.com/golang-jwt/jwt/v5"
	"github.com/synctv-org/synctv/internal/conf"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
)

var s *rtmps.Server

type Claims struct {
	MovieID string `json:"m"`
	jwt.RegisteredClaims
}

func AuthRtmpPublish(authorization string) (movieID string, err error) {
	t, err := jwt.ParseWithClaims(
		strings.TrimPrefix(authorization, `Bearer `),
		&Claims{},
		func(_ *jwt.Token) (any, error) {
			return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
		},
	)
	if err != nil {
		return "", errors.New("auth failed")
	}

	claims, ok := t.Claims.(*Claims)
	if !ok {
		return "", errors.New("auth failed")
	}

	return claims.MovieID, nil
}

func NewRtmpAuthorization(movieID string) (string, error) {
	claims := &Claims{
		MovieID: movieID,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
		},
	}

	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).
		SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func Init(rs *rtmps.Server) {
	s = rs
}

func Server() *rtmps.Server {
	return s
}
