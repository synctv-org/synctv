package middlewares

import (
	"errors"
	"net/http"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
	"github.com/zijiren233/stream"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
)

type AuthClaims struct {
	UserId string `json:"u"`
	jwt.RegisteredClaims
}

type AuthRoomClaims struct {
	AuthClaims
	RoomId  string `json:"r"`
	Version uint32 `json:"rv"`
}

func authRoom(Authorization string) (*AuthRoomClaims, error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &AuthRoomClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil {
		return nil, ErrAuthFailed
	}
	claims, ok := t.Claims.(*AuthRoomClaims)
	if !ok || !t.Valid {
		return nil, ErrAuthFailed
	}
	return claims, nil
}

func authUser(Authorization string) (*AuthClaims, error) {
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

func AuthRoom(Authorization string) (*op.User, *op.Room, error) {
	claims, err := authRoom(Authorization)
	if err != nil {
		return nil, nil, err
	}

	if len(claims.RoomId) != 32 {
		return nil, nil, ErrAuthFailed
	}

	if len(claims.UserId) != 32 {
		return nil, nil, ErrAuthFailed
	}

	u, err := op.GetUserById(claims.UserId)
	if err != nil {
		return nil, nil, err
	}

	r, err := op.LoadOrInitRoomByID(claims.RoomId)
	if err != nil {
		return nil, nil, err
	}
	if !r.CheckVersion(claims.Version) {
		return nil, nil, ErrAuthExpired
	}

	return u, r, nil
}

func AuthUser(Authorization string) (*op.User, error) {
	claims, err := authUser(Authorization)
	if err != nil {
		return nil, err
	}

	if len(claims.UserId) != 32 {
		return nil, ErrAuthFailed
	}

	u, err := op.GetUserById(claims.UserId)
	if err != nil {
		return nil, err
	}

	return u, nil
}

func NewAuthUserToken(user *op.User) (string, error) {
	if user.IsBanned() {
		return "", errors.New("user banned")
	}
	if user.IsPending() {
		return "", errors.New("user is pending, need admin to approve")
	}
	t, err := time.ParseDuration(conf.Conf.Jwt.Expire)
	if err != nil {
		return "", err
	}
	claims := &AuthClaims{
		UserId: user.ID,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
			ExpiresAt: jwt.NewNumericDate(time.Now().Add(t)),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func NewAuthRoomToken(user *op.User, room *op.Room) (string, error) {
	if user.IsBanned() {
		return "", errors.New("user banned")
	}
	if user.IsPending() {
		return "", errors.New("user is pending, need admin to approve")
	}
	if room.IsBanned() {
		return "", errors.New("room banned")
	}
	if room.IsPending() {
		return "", errors.New("room is pending, need admin to approve")
	}
	if room.Settings.DisableJoinNewUser {
		if _, err := room.GetRoomUserRelation(user.ID); err != nil {
			return "", errors.New("room is not allow new user to join")
		}
	} else if _, err := room.LoadOrCreateRoomUserRelation(user.ID); err != nil {
		return "", err
	}

	t, err := time.ParseDuration(conf.Conf.Jwt.Expire)
	if err != nil {
		return "", err
	}
	claims := &AuthRoomClaims{
		AuthClaims: AuthClaims{
			UserId: user.ID,
			RegisteredClaims: jwt.RegisteredClaims{
				NotBefore: jwt.NewNumericDate(time.Now()),
				ExpiresAt: jwt.NewNumericDate(time.Now().Add(t)),
			},
		},
		RoomId:  room.ID,
		Version: room.Version(),
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func AuthRoomMiddleware(ctx *gin.Context) {
	user, room, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}

	ctx.Set("user", user)
	ctx.Set("room", room)
	ctx.Next()
}

func AuthUserMiddleware(ctx *gin.Context) {
	user, err := AuthUser(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}

	ctx.Set("user", user)
	ctx.Next()
}

func AuthAdminMiddleware(ctx *gin.Context) {
	user, err := AuthUser(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	if !user.IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is not admin"))
		return
	}

	ctx.Set("user", user)
	ctx.Next()
}

func AuthRootMiddleware(ctx *gin.Context) {
	user, err := AuthUser(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is not root"))
		return
	}

	ctx.Set("user", user)
	ctx.Next()
}
