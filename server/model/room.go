package model

import (
	"errors"
	"fmt"
	"regexp"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"

	"github.com/gin-gonic/gin"
)

var (
	ErrEmptyRoomId          = errors.New("empty room id")
	ErrRoomIdTooLong        = errors.New("room id too long")
	ErrRoomIdHasInvalidChar = errors.New("room id has invalid char")

	ErrPasswordTooLong        = errors.New("password too long")
	ErrPasswordHasInvalidChar = errors.New("password has invalid char")

	ErrEmptyUserId            = errors.New("empty user id")
	ErrEmptyUsername          = errors.New("empty username")
	ErrUsernameTooLong        = errors.New("username too long")
	ErrUsernameHasInvalidChar = errors.New("username has invalid char")
)

var (
	alphaNumReg        = regexp.MustCompile(`^[a-zA-Z0-9_\-]+$`)
	alphaNumChineseReg = regexp.MustCompile(`^[\p{Han}a-zA-Z0-9_\-]+$`)
)

type FormatEmptyPasswordError string

func (f FormatEmptyPasswordError) Error() string {
	return fmt.Sprintf("%s password empty", string(f))
}

type CreateRoomReq struct {
	RoomId   string        `json:"roomId"`
	Password string        `json:"password"`
	Setting  model.Setting `json:"setting"`
}

func (c *CreateRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(c)
}

func (c *CreateRoomReq) Validate() error {
	if c.RoomId == "" {
		return ErrEmptyRoomId
	} else if len(c.RoomId) > 32 {
		return ErrRoomIdTooLong
	} else if !alphaNumChineseReg.MatchString(c.RoomId) {
		return ErrRoomIdHasInvalidChar
	}

	if c.Password != "" {
		if len(c.Password) > 32 {
			return ErrPasswordTooLong
		} else if !alphaNumReg.MatchString(c.Password) {
			return ErrPasswordHasInvalidChar
		}
	} else if conf.Conf.Room.MustPassword {
		return FormatEmptyPasswordError("room")
	}

	return nil
}

type RoomListResp struct {
	RoomId       uint   `json:"roomId"`
	RoomName     string `json:"roomName"`
	PeopleNum    int64  `json:"peopleNum"`
	NeedPassword bool   `json:"needPassword"`
	Creator      string `json:"creator"`
	CreatedAt    int64  `json:"createdAt"`
}

type LoginRoomReq struct {
	RoomId   uint   `json:"roomId"`
	Password string `json:"password"`
}

func (l *LoginRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginRoomReq) Validate() error {
	if l.RoomId == 0 {
		return ErrEmptyRoomId
	}

	return nil
}

type SetRoomPasswordReq struct {
	Password string `json:"password"`
}

func (s *SetRoomPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SetRoomPasswordReq) Validate() error {
	if len(s.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alphaNumReg.MatchString(s.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type UserIdReq struct {
	UserId uint `json:"userId"`
}

func (u *UserIdReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserIdReq) Validate() error {
	if u.UserId == 0 {
		return ErrEmptyUserId
	}
	return nil
}
