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
	if err != nil {
		return nil, ErrAuthFailed
	}
	claims, ok := t.Claims.(*AuthClaims)
	if !ok || !t.Valid {
		return nil, ErrAuthFailed
	}
	return claims, nil
}

func AuthRoom(Authorization, roomId string) (*op.UserEntry, *op.RoomEntry, error) {
	if len(roomId) != 32 {
		return nil, nil, ErrAuthFailed
	}

	claims, err := authUser(Authorization)
	if err != nil {
		return nil, nil, err
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

	if user.IsBanned() {
		return nil, nil, fmt.Errorf("user is banned")
	}
	if user.IsPending() {
		return nil, nil, fmt.Errorf("user is pending, need admin to approve")
	}

	roomE, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		return nil, nil, err
	}
	room := roomE.Value()

	if !room.NeedPassword() && room.IsGuest(user.ID) {
		return nil, nil, fmt.Errorf("guests are not allowed to join rooms that require a password")
	}

	if room.IsBanned() {
		return nil, nil, fmt.Errorf("room is banned")
	}
	if room.IsPending() {
		return nil, nil, fmt.Errorf("room is pending, need admin to approve")
	}

	rus, err := room.LoadMemberStatus(user.ID)
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
	roomId, err := GetRoomIdFromContext(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, model.NewApiErrorResp(err))
		return
	}
	userE, roomE, err := AuthRoom(token, roomId)
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

func GetAuthorizationTokenFromContext(ctx *gin.Context) (Authorization string, err error) {
	Authorization = ctx.GetHeader("Authorization")
	defer func() {
		if err != nil && Authorization != "" {
			ctx.Set("token", Authorization)
		}
	}()
	if Authorization != "" {
		return Authorization, nil
	}
	if ctx.IsWebsocket() {
		Authorization = ctx.GetHeader("Sec-WebSocket-Protocol")
		if Authorization != "" {
			return Authorization, nil
		}
	}
	Authorization = ctx.Query("token")
	if Authorization != "" {
		return Authorization, nil
	}
	return "", errors.New("token is empty")
}

func GetRoomIdFromContext(ctx *gin.Context) (string, error) {
	roomID := ctx.Param("roomId")
	if len(roomID) == 32 {
		return roomID, nil
	}
	roomID = ctx.Query("roomId")
	if len(roomID) == 32 {
		return roomID, nil
	}
	return "", errors.New("room id length is not 32")
}
