package model

import (
	"errors"
	"fmt"
	"regexp"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/op"

	"github.com/gin-gonic/gin"
	dbModel "github.com/synctv-org/synctv/internal/model"
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
	RoomName string               `json:"roomName"`
	Password string               `json:"password"`
	Setting  dbModel.RoomSettings `json:"setting"`
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
	}

	return nil
}

type RoomListResp = op.RoomInfo

type LoginRoomReq struct {
	RoomId   string `json:"roomId"`
	Password string `json:"password"`
}

func (l *LoginRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginRoomReq) Validate() error {
	if l.RoomId == "" {
		return ErrEmptyRoomName
	} else if len(l.RoomId) != 32 {
		return errors.New("invalid room id")
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
	if s.Password != "" {
		if len(s.Password) > 32 {
			return ErrPasswordTooLong
		} else if !alnumPrintReg.MatchString(s.Password) {
			return ErrPasswordHasInvalidChar
		}
	}
	return nil
}

type RoomIDReq struct {
	Id string `json:"id"`
}

func (r *RoomIDReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func (r *RoomIDReq) Validate() error {
	if len(r.Id) != 32 {
		return ErrEmptyRoomName
	}

	return nil
}

type SetRoomSettingReq dbModel.RoomSettings

func (s *SetRoomSettingReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SetRoomSettingReq) Validate() error {
	return nil
}

type RoomUsersResp struct {
	UserID      string                     `json:"userId"`
	Username    string                     `json:"username"`
	Role        dbModel.Role               `json:"role"`
	JoinAt      int64                      `json:"joinAt"`
	RoomID      string                     `json:"roomId"`
	Status      dbModel.RoomUserStatus     `json:"status"`
	Permissions dbModel.RoomUserPermission `json:"permissions"`
}
