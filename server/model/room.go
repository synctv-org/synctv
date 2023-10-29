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
	ErrEmptyRoomName          = errors.New("empty room name")
	ErrRoomNameTooLong        = errors.New("room name too long")
	ErrRoomNameHasInvalidChar = errors.New("room name has invalid char")

	ErrPasswordTooLong        = errors.New("password too long")
	ErrPasswordHasInvalidChar = errors.New("password has invalid char")

	ErrEmptyUserId            = errors.New("empty user id")
	ErrEmptyUsername          = errors.New("empty username")
	ErrUsernameTooLong        = errors.New("username too long")
	ErrUsernameHasInvalidChar = errors.New("username has invalid char")
)

var (
	alnumReg         = regexp.MustCompile(`^[[:alnum:]]+$`)
	alnumPrintReg    = regexp.MustCompile(`^[[:print:][:alnum:]]+$`)
	alnumPrintHanReg = regexp.MustCompile(`^[[:print:][:alnum:]\p{Han}]+$`)
)

type FormatEmptyPasswordError string

func (f FormatEmptyPasswordError) Error() string {
	return fmt.Sprintf("%s password empty", string(f))
}

type CreateRoomReq struct {
	RoomName string         `json:"roomName"`
	Password string         `json:"password"`
	Setting  model.Settings `json:"setting"`
}

func (c *CreateRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(c)
}

func (c *CreateRoomReq) Validate() error {
	if c.RoomName == "" {
		return ErrEmptyRoomName
	} else if len(c.RoomName) > 32 {
		return ErrRoomNameTooLong
	} else if !alnumPrintHanReg.MatchString(c.RoomName) {
		return ErrRoomNameHasInvalidChar
	}

	if c.Password != "" {
		if len(c.Password) > 32 {
			return ErrPasswordTooLong
		} else if !alnumPrintReg.MatchString(c.Password) {
			return ErrPasswordHasInvalidChar
		}
	} else if conf.Conf.Room.MustPassword {
		return FormatEmptyPasswordError("room")
	}

	return nil
}

type RoomListResp struct {
	RoomId       string `json:"roomId"`
	RoomName     string `json:"roomName"`
	PeopleNum    int64  `json:"peopleNum"`
	NeedPassword bool   `json:"needPassword"`
	Creator      string `json:"creator"`
	CreatedAt    int64  `json:"createdAt"`
}

type LoginRoomReq struct {
	RoomId   string `json:"roomId"`
	Password string `json:"password"`
}

func (l *LoginRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginRoomReq) Validate() error {
	if len(l.RoomId) != 36 {
		return ErrEmptyRoomName
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
	} else if !alnumPrintReg.MatchString(s.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type UserIdReq struct {
	UserId string `json:"userId"`
}

func (u *UserIdReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(u)
}

func (u *UserIdReq) Validate() error {
	if len(u.UserId) != 36 {
		return ErrEmptyUserId
	}
	return nil
}
