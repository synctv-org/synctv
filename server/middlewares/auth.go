package middlewares

import (
	"errors"
	"fmt"
	"net/http"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
	"github.com/zijiren233/gencontainer/synccache"
	"github.com/zijiren233/stream"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
)

type AuthClaims struct {
	UserId      string `json:"u"`
	UserVersion uint32 `json:"uv"`
	jwt.RegisteredClaims
}

type AuthRoomClaims struct {
	AuthClaims
	RoomId      string `json:"r"`
	RoomVersion uint32 `json:"rv"`
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

func AuthRoom(Authorization string) (*op.UserEntry, *op.RoomEntry, error) {
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

	userE, err := op.LoadOrInitUserByID(claims.UserId)
	if err != nil {
		return nil, nil, err
	}
	user := userE.Value()

	if !user.CheckVersion(claims.UserVersion) {
		return nil, nil, ErrAuthExpired
	}

	roomE, err := op.LoadOrInitRoomByID(claims.RoomId)
	if err != nil {
		return nil, nil, err
	}
	room := roomE.Value()

	if !room.CheckVersion(claims.RoomVersion) {
		return nil, nil, ErrAuthExpired
	}

	rus, err := room.LoadOrCreateMemberStatus(user.ID)
	if err != nil {
		return nil, nil, err
	}
	if !rus.IsActive() {
		if rus.IsPending() {
			return nil, nil, fmt.Errorf("user is pending, need admin to approve")
		}
		return nil, nil, fmt.Errorf("user is banned")
	}

	return userE, roomE, nil
}

func AuthUser(Authorization string) (*op.UserEntry, error) {
	claims, err := authUser(Authorization)
	if err != nil {
		return nil, err
	}

	if len(claims.UserId) != 32 {
		return nil, ErrAuthFailed
	}

	u, err := op.LoadOrInitUserByID(claims.UserId)
	if err != nil {
		return nil, err
	}

	if !u.Value().CheckVersion(claims.UserVersion) {
		return nil, ErrAuthExpired
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
		UserId:      user.ID,
		UserVersion: user.Version(),
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

	if _, err := room.LoadOrCreateRoomMember(user.ID); err != nil {
		if errors.Is(err, db.ErrNotFound("")) {
			return "", fmt.Errorf("this room was disabled join new user")
		}
		return "", fmt.Errorf("load room member failed: %w", err)
	}

	t, err := time.ParseDuration(conf.Conf.Jwt.Expire)
	if err != nil {
		return "", fmt.Errorf("parse jwt expire failed: %w", err)
	}
	claims := &AuthRoomClaims{
		AuthClaims: AuthClaims{
			UserId:      user.ID,
			UserVersion: user.Version(),
			RegisteredClaims: jwt.RegisteredClaims{
				NotBefore: jwt.NewNumericDate(time.Now()),
				ExpiresAt: jwt.NewNumericDate(time.Now().Add(t)),
			},
		},
		RoomId:      room.ID,
		RoomVersion: room.Version(),
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func AuthUserMiddleware(ctx *gin.Context) {
	token, err := GetAuthorizationTokenFromContext(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	userE, err := AuthUser(token)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	user := userE.Value()
	if user.IsBanned() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user banned"))
		return
	}
	if user.IsPending() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is pending, need admin to approve"))
		return
	}

	ctx.Set("user", userE)
	log := ctx.MustGet("log").(*logrus.Entry)
	if log.Data == nil {
		log.Data = make(logrus.Fields, 3)
	}
	log.Data["uid"] = user.ID
	log.Data["unm"] = user.Username
	log.Data["uro"] = user.Role.String()
}

func AuthRoomMiddleware(ctx *gin.Context) {
	token, err := GetAuthorizationTokenFromContext(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	userE, roomE, err := AuthRoom(token)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}

	user := userE.Value()
	if user.IsBanned() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user banned"))
		return
	}
	if user.IsPending() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is pending, need admin to approve"))
		return
	}

	room := roomE.Value()
	if room.IsBanned() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("room banned"))
		return
	}
	if room.IsPending() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("room is pending, need admin to approve"))
		return
	}

	ctx.Set("user", userE)
	ctx.Set("room", roomE)
	log := ctx.MustGet("log").(*logrus.Entry)
	if log.Data == nil {
		log.Data = make(logrus.Fields, 5)
	}
	log.Data["rid"] = room.ID
	log.Data["rnm"] = room.Name
	log.Data["uid"] = user.ID
	log.Data["unm"] = user.Username
	log.Data["uro"] = user.Role.String()
}

func AuthRoomAdminMiddleware(ctx *gin.Context) {
	AuthRoomMiddleware(ctx)
	if ctx.IsAborted() {
		return
	}

	room := ctx.MustGet("room").(*synccache.Entry[*op.Room]).Value()
	user := ctx.MustGet("user").(*synccache.Entry[*op.User]).Value()

	if !user.IsRoomAdmin(room) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user has no permission"))
		return
	}
}

func AuthRoomCreatorMiddleware(ctx *gin.Context) {
	AuthRoomMiddleware(ctx)
	if ctx.IsAborted() {
		return
	}

	room := ctx.MustGet("room").(*synccache.Entry[*op.Room]).Value()
	user := ctx.MustGet("user").(*synccache.Entry[*op.User]).Value()

	if room.CreatorID != user.ID {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is not creator"))
		return
	}
}

func AuthAdminMiddleware(ctx *gin.Context) {
	AuthUserMiddleware(ctx)
	if ctx.IsAborted() {
		return
	}

	userE := ctx.MustGet("user").(*synccache.Entry[*op.User])
	if !userE.Value().IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is not admin"))
		return
	}
}

func AuthRootMiddleware(ctx *gin.Context) {
	AuthUserMiddleware(ctx)
	if ctx.IsAborted() {
		return
	}

	userE := ctx.MustGet("user").(*synccache.Entry[*op.User])
	if !userE.Value().IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("user is not root"))
		return
	}
}

func GetAuthorizationTokenFromContext(ctx *gin.Context) (string, error) {
	Authorization := ctx.GetHeader("Authorization")
	if Authorization != "" {
		return Authorization, nil
	}
	Authorization = ctx.Query("token")
	if Authorization != "" {
		return Authorization, nil
	}
	return "", errors.New("token is empty")
}
