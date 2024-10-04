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
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/settings"
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

func authUser(Authorization string) (*AuthClaims, error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &AuthClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil || !t.Valid {
		return nil, ErrAuthFailed
	}
	claims, ok := t.Claims.(*AuthClaims)
	if !ok {
		return nil, ErrAuthFailed
	}
	return claims, nil
}

func AuthRoom(authorization, roomId string) (*op.UserEntry, *op.RoomEntry, error) {
	if len(roomId) != 32 {
		return nil, nil, ErrAuthFailed
	}

	var userE *op.UserEntry
	var err error

	if authorization != "" {
		userE, err = authenticateUser(authorization)
	} else {
		userE, err = authenticateGuest()
	}
	if err != nil {
		return nil, nil, err
	}

	user := userE.Value()
	roomE, err := authenticateRoomAccess(roomId, user)
	if err != nil {
		return nil, nil, err
	}

	return userE, roomE, nil
}

func authenticateUser(Authorization string) (*op.UserEntry, error) {
	claims, err := authUser(Authorization)
	if err != nil {
		return nil, err
	}

	if len(claims.UserId) != 32 {
		return nil, ErrAuthFailed
	}

	userE, err := op.LoadOrInitUserByID(claims.UserId)
	if err != nil {
		return nil, err
	}
	user := userE.Value()

	if user.IsGuest() {
		return nil, fmt.Errorf("guests are not allowed to join rooms by token")
	}

	if !user.CheckVersion(claims.UserVersion) {
		return nil, ErrAuthExpired
	}
	if user.IsBanned() {
		return nil, fmt.Errorf("user is banned")
	}
	if user.IsPending() {
		return nil, fmt.Errorf("user is pending, need admin to approve")
	}

	return userE, nil
}

func authenticateGuest() (*op.UserEntry, error) {
	if !settings.EnableGuest.Get() {
		return nil, fmt.Errorf("guests is disabled")
	}
	return op.LoadOrInitGuestUser()
}

func authenticateRoomAccess(roomId string, user *op.User) (*op.RoomEntry, error) {
	roomE, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		return nil, err
	}
	room := roomE.Value()

	if room.IsGuest(user.ID) {
		if room.Settings.DisableGuest {
			return nil, fmt.Errorf("guests are not allowed to join rooms")
		}
		if room.NeedPassword() {
			return nil, fmt.Errorf("guests are not allowed to join rooms that require a password")
		}
	}

	if room.IsBanned() {
		return nil, fmt.Errorf("room is banned")
	}
	if room.IsPending() {
		return nil, fmt.Errorf("room is pending, need admin to approve")
	}

	rus, err := room.LoadMemberStatus(user.ID)
	if err != nil {
		return nil, err
	}
	if !rus.IsActive() {
		if rus.IsPending() {
			return nil, fmt.Errorf("user is pending, need admin to approve")
		}
		return nil, fmt.Errorf("user is banned")
	}

	return roomE, nil
}

func AuthUser(Authorization string) (*op.UserEntry, error) {
	claims, err := authUser(Authorization)
	if err != nil {
		return nil, err
	}

	if len(claims.UserId) != 32 {
		return nil, ErrAuthFailed
	}

	userE, err := op.LoadOrInitUserByID(claims.UserId)
	if err != nil {
		return nil, err
	}
	user := userE.Value()

	if user.IsGuest() {
		return nil, errors.New("user is guest, can not login")
	}

	if !user.CheckVersion(claims.UserVersion) {
		return nil, ErrAuthExpired
	}

	if user.IsBanned() {
		return nil, errors.New("user is banned")
	}
	if user.IsPending() {
		return nil, errors.New("user is pending, need admin to approve")
	}

	return userE, nil
}

func NewAuthUserToken(user *op.User) (string, error) {
	if user.IsBanned() {
		return "", errors.New("user banned")
	}
	if user.IsPending() {
		return "", errors.New("user is pending, need admin to approve")
	}
	if user.IsGuest() {
		return "", errors.New("user is guest, can not login")
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

func AuthUserMiddleware(ctx *gin.Context) {
	token := GetAuthorizationTokenFromContext(ctx)
	if token == "" {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorStringResp("token is empty"))
		return
	}
	userE, err := AuthUser(token)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	user := userE.Value()

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
	roomId, err := GetRoomIdFromContext(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	userE, roomE, err := AuthRoom(GetAuthorizationTokenFromContext(ctx), roomId)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	user := userE.Value()
	room := roomE.Value()

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

func AuthRoomWithoutGuestMiddleware(ctx *gin.Context) {
	AuthRoomMiddleware(ctx)
	if ctx.IsAborted() {
		return
	}

	user := ctx.MustGet("user").(*synccache.Entry[*op.User]).Value()
	if user.IsGuest() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("guest is no permission"))
		return
	}
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

func GetAuthorizationTokenFromContext(ctx *gin.Context) string {
	if token := ctx.GetHeader("Authorization"); token != "" {
		ctx.Set("token", token)
		return token
	}

	if ctx.IsWebsocket() {
		if token := ctx.GetHeader("Sec-WebSocket-Protocol"); token != "" {
			ctx.Set("token", token)
			return token
		}
	}

	if token := ctx.Query("token"); token != "" {
		ctx.Set("token", token)
		return token
	}

	ctx.Set("token", "")
	return ""
}

func GetRoomIdFromContext(ctx *gin.Context) (string, error) {
	sources := []func() string{
		func() string { return ctx.GetHeader("X-Room-Id") },
		func() string { return ctx.Query("roomId") },
		func() string { return ctx.Param("roomId") },
	}

	for _, source := range sources {
		roomId := source()
		if roomId == "" {
			continue
		}
		if len(roomId) == 32 {
			ctx.Set("roomId", roomId)
			return roomId, nil
		}
		ctx.Set("roomId", "")
		return "", errors.New("room id length is not 32")
	}

	ctx.Set("roomId", "")
	return "", errors.New("room id is empty")
}
