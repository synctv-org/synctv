package middlewares

import (
	"errors"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/server/model"
	"github.com/zijiren233/stream"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
)

type AuthClaims struct {
	RoomId      string `json:"id"`
	Version     uint64 `json:"v"`
	Username    string `json:"un"`
	UserVersion uint64 `json:"uv"`
	jwt.RegisteredClaims
}

func auth(Authorization string) (*AuthClaims, error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &AuthClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil {
		return nil, ErrAuthFailed
	}
	claims, ok := t.Claims.(*AuthClaims)
	if !ok || !t.Valid {
		return nil, ErrAuthFailed
	}
	return claims, nil
}

func Auth(Authorization string, rooms *room.Rooms) (*room.User, error) {
	claims, err := auth(Authorization)
	if err != nil {
		return nil, err
	}
	r, err := rooms.GetRoom(claims.RoomId)
	if err != nil {
		return nil, err
	}

	if !r.CheckVersion(claims.Version) {
		return nil, ErrAuthExpired
	}

	user, err := r.GetUser(claims.Username)
	if err != nil {
		return nil, err
	}

	if !user.CheckVersion(claims.UserVersion) {
		return nil, ErrAuthExpired
	}

	return user, nil
}

func AuthWithPassword(roomId, roomPassword, username, password string, rooms *room.Rooms) (*room.User, error) {
	room, err := rooms.GetRoom(roomId)
	if err != nil {
		return nil, err
	}
	if !room.CheckPassword(roomPassword) {
		return nil, ErrAuthFailed
	}
	user, err := room.GetUser(username)
	if err != nil {
		return nil, err
	}
	if !user.CheckPassword(password) {
		return nil, ErrAuthFailed
	}
	return user, nil
}

func AuthOrNewWithPassword(roomId, roomPassword, username, password string, rooms *room.Rooms) (*room.User, error) {
	room, err := rooms.GetRoom(roomId)
	if err != nil {
		return nil, err
	}
	if !room.CheckPassword(roomPassword) {
		return nil, ErrAuthFailed
	}
	user, err := room.GetOrNewUser(username, password)
	if err != nil {
		return nil, err
	}
	if !user.CheckPassword(password) {
		return nil, ErrAuthFailed
	}
	return user, nil
}

func AuthRoom(ctx *gin.Context) {
	rooms := ctx.Value("rooms").(*room.Rooms)
	user, err := Auth(ctx.GetHeader("Authorization"), rooms)
	if err != nil {
		ctx.AbortWithStatusJSON(401, model.NewApiErrorResp(err))
		return
	}

	ctx.Set("user", user)
	ctx.Next()
}

func NewAuthToken(user *room.User) (string, error) {
	claims := &AuthClaims{
		RoomId:      user.Room().Id(),
		Version:     user.Room().Version(),
		Username:    user.Name(),
		UserVersion: user.Version(),
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
			ExpiresAt: jwt.NewNumericDate(time.Now().Add(time.Hour * time.Duration(conf.Conf.Jwt.Expire))),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}
