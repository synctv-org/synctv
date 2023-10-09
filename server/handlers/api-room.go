package handlers

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
	"time"

	json "github.com/json-iterator/go"
	log "github.com/sirupsen/logrus"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/maruel/natural"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/room"
	"github.com/zijiren233/gencontainer/vec"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
)

var (
	ErrAuthFailed  = errors.New("auth failed")
	ErrAuthExpired = errors.New("auth expired")
	ErrRoomAlready = errors.New("room already exists")
)

type FormatErrNotSupportPosition string

func (e FormatErrNotSupportPosition) Error() string {
	return fmt.Sprintf("not support position %s", string(e))
}

type AuthClaims struct {
	RoomID      string `json:"id"`
	Version     uint64 `json:"v"`
	Username    string `json:"un"`
	UserVersion uint64 `json:"uv"`
	jwt.RegisteredClaims
}

func AuthRoom(Authorization string) (*room.User, error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &AuthClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil {
		return nil, ErrAuthFailed
	}
	claims := t.Claims.(*AuthClaims)

	r, err := Rooms.GetRoom(claims.RoomID)
	if err != nil {
		return nil, err
	}

	if !r.CheckVersion(claims.Version) {
		return nil, ErrAuthExpired
	}

	user, err := r.GetUser(claims.Username)
	if err != nil {
		return nil, ErrUserNotFound
	}

	if !user.CheckVersion(claims.UserVersion) {
		return nil, ErrAuthExpired
	}

	return user, nil
}

func authWithPassword(roomid, password, username, userPassword string) (*room.User, error) {
	r, err := Rooms.GetRoom(roomid)
	if err != nil {
		return nil, err
	}
	if !r.CheckPassword(password) {
		return nil, ErrAuthFailed
	}
	user, err := r.GetUser(username)
	if err != nil {
		return nil, ErrUserNotFound
	}
	if !user.CheckPassword(userPassword) {
		return nil, ErrAuthFailed
	}
	return user, nil
}

func authOrNewWithPassword(roomid, password, username, userPassword string, conf ...room.UserConf) (*room.User, error) {
	r, err := Rooms.GetRoom(roomid)
	if err != nil {
		return nil, err
	}
	if !r.CheckPassword(password) {
		return nil, ErrAuthFailed
	}
	user, err := r.GetOrNewUser(username, userPassword, conf...)
	if err != nil {
		return nil, err
	}
	if !user.CheckPassword(userPassword) {
		return nil, ErrAuthFailed
	}
	return user, nil
}

func newAuthorization(user *room.User) (string, error) {
	claims := &AuthClaims{
		RoomID:      user.Room().ID(),
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

type CreateRoomReq struct {
	RoomID       string `json:"roomId"`
	Password     string `json:"password"`
	Username     string `json:"username"`
	UserPassword string `json:"userPassword"`
	Hidden       bool   `json:"hidden"`
}

func NewCreateRoomHandler(s *rtmps.Server) gin.HandlerFunc {
	return func(ctx *gin.Context) {
		req := new(CreateRoomReq)
		if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
			return
		}

		user, err := room.NewUser(req.Username, req.UserPassword, nil, room.WithUserAdmin(true))
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
			return
		}

		r, err := Rooms.CreateRoom(req.RoomID, req.Password, s,
			room.WithHidden(req.Hidden),
			room.WithRootUser(user),
		)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
			return
		}

		token, err := newAuthorization(user)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
			return
		}

		r.Init()
		r.Start()

		go func() {
			ticker := time.NewTicker(time.Second * 5)
			defer ticker.Stop()
			var pre int64 = 0
			for range ticker.C {
				if r.Closed() {
					log.Infof("ws: room %s closed, stop broadcast people num", r.ID())
					return
				}
				current := r.ClientNum()
				if current != pre {
					if err := r.Broadcast(&room.ElementMessage{
						Type:      room.ChangePeopleNum,
						PeopleNum: current,
					}); err != nil {
						log.Errorf("ws: room %s broadcast people num error: %v", r.ID(), err)
						continue
					}
					pre = current
				} else {
					if err := r.Broadcast(&room.PingMessage{}); err != nil {
						log.Errorf("ws: room %s broadcast ping error: %v", r.ID(), err)
						continue
					}
				}
			}
		}()

		ctx.JSON(http.StatusCreated, NewApiDataResp(gin.H{
			"token": token,
		}))
	}
}

type RoomListResp struct {
	RoomID       string `json:"roomId"`
	PeopleNum    int64  `json:"peopleNum"`
	NeedPassword bool   `json:"needPassword"`
	Creator      string `json:"creator"`
	CreateAt     int64  `json:"createAt"`
}

func RoomList(ctx *gin.Context) {
	r := Rooms.ListNonHidden()
	resp := vec.New[*RoomListResp](vec.WithCmpLess[*RoomListResp](func(v1, v2 *RoomListResp) bool {
		return v1.PeopleNum < v2.PeopleNum
	}), vec.WithCmpEqual[*RoomListResp](func(v1, v2 *RoomListResp) bool {
		return v1.PeopleNum == v2.PeopleNum
	}))
	for _, v := range r {
		resp.Push(&RoomListResp{
			RoomID:       v.ID(),
			PeopleNum:    v.ClientNum(),
			NeedPassword: v.NeedPassword(),
			Creator:      v.RootUser().Name(),
			CreateAt:     v.CreateAt(),
		})
	}

	switch ctx.DefaultQuery("sort", "peopleNum") {
	case "peopleNum":
		resp.SortStable()
	case "creator":
		resp.SortStableFunc(func(v1, v2 *RoomListResp) bool {
			return natural.Less(v1.Creator, v2.Creator)
		}, func(t1, t2 *RoomListResp) bool {
			return t1.Creator == t2.Creator
		})
	case "createAt":
		resp.SortStableFunc(func(v1, v2 *RoomListResp) bool {
			return v1.CreateAt < v2.CreateAt
		}, func(t1, t2 *RoomListResp) bool {
			return t1.CreateAt == t2.CreateAt
		})
	case "roomId":
		resp.SortStableFunc(func(v1, v2 *RoomListResp) bool {
			return natural.Less(v1.RoomID, v2.RoomID)
		}, func(t1, t2 *RoomListResp) bool {
			return t1.RoomID == t2.RoomID
		})
	case "needPassword":
		resp.SortStableFunc(func(v1, v2 *RoomListResp) bool {
			return v1.NeedPassword && !v2.NeedPassword
		}, func(t1, t2 *RoomListResp) bool {
			return t1.NeedPassword == t2.NeedPassword
		})
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("sort must be peoplenum or roomid"))
		return
	}

	switch ctx.DefaultQuery("order", "desc") {
	case "asc":
		// do nothing
	case "desc":
		resp.Reverse()
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("order must be asc or desc"))
		return
	}

	list, err := GetPageItems(ctx, resp.Slice())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"total": resp.Len(),
		"list":  list,
	}))
}

func CheckRoom(ctx *gin.Context) {
	r, err := Rooms.GetRoom(ctx.Query("roomId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"peopleNum":    r.ClientNum(),
		"needPassword": r.NeedPassword(),
	}))
}

func CheckUser(ctx *gin.Context) {
	r, err := Rooms.GetRoom(ctx.Query("roomId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}

	u, err := r.GetUser(ctx.Query("username"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"idRoot":  u.IsRoot(),
		"idAdmin": u.IsAdmin(),
		"lastAct": u.LastAct(),
	}))
}

type LoginRoomReq struct {
	RoomID       string `json:"roomId"`
	Password     string `json:"password"`
	Username     string `json:"username"`
	UserPassword string `json:"userPassword"`
}

func LoginRoom(ctx *gin.Context) {
	req := new(LoginRoomReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	autoNew, err := strconv.ParseBool(ctx.DefaultQuery("autoNew", "false"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("autoNew must be bool"))
		return
	}

	var (
		user *room.User
	)
	if autoNew {
		user, err = authOrNewWithPassword(req.RoomID, req.Password, req.Username, req.UserPassword)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
			return
		}
	} else {
		user, err = authWithPassword(req.RoomID, req.Password, req.Username, req.UserPassword)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
			return
		}
	}

	token, err := newAuthorization(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"token": token,
	}))
}

func DeleteRoom(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorStringResp("only root can close room"))
		return
	}

	err = Rooms.DelRoom(user.Room().ID())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type SetPasswordReq struct {
	Password string `json:"password"`
}

func SetPassword(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if !user.IsRoot() || !user.IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorStringResp("only root or admin can set password"))
		return
	}

	req := new(SetPasswordReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	user.Room().SetPassword(req.Password)

	token, err := newAuthorization(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"token": token,
	}))
}

type UsernameReq struct {
	Username string `json:"username"`
}

func AddAdmin(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if !user.IsRoot() && !user.IsAdmin() {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorStringResp("only root or admin can add admin"))
		return
	}

	req := new(UsernameReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	u, err := user.Room().GetUser(req.Username)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}

	u.SetAdmin(true)

	ctx.Status(http.StatusNoContent)
}

func DelAdmin(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorStringResp("only root can del admin"))
		return
	}

	req := new(UsernameReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	u, err := user.Room().GetUser(req.Username)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}

	u.SetAdmin(false)

	ctx.Status(http.StatusNoContent)
}

func CloseRoom(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if !user.IsRoot() {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorStringResp("only root can close room"))
		return
	}

	err = Rooms.DelRoom(user.Room().ID())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
