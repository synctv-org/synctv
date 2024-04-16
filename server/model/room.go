package model

import (
	"errors"
	"fmt"

	json "github.com/json-iterator/go"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/model"
)

var (
	ErrEmptyRoomName          = errors.New("empty room name")
	ErrRoomNameTooLong        = errors.New("room name too long")
	ErrRoomNameHasInvalidChar = errors.New("room name has invalid char")

	ErrPasswordTooLong        = errors.New("password too long")
	ErrPasswordHasInvalidChar = errors.New("password has invalid char")
)

type FormatEmptyPasswordError string

func (f FormatEmptyPasswordError) Error() string {
	return fmt.Sprintf("%s password empty", string(f))
}

type CreateRoomReq struct {
	RoomName string `json:"roomName"`
	Password string `json:"password"`
	Settings struct {
		Hidden bool `json:"hidden"`
	} `json:"settings"`
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

type RoomListResp struct {
	RoomId       string           `json:"roomId"`
	RoomName     string           `json:"roomName"`
	PeopleNum    int64            `json:"peopleNum"`
	NeedPassword bool             `json:"needPassword"`
	CreatorID    string           `json:"creatorId"`
	Creator      string           `json:"creator"`
	CreatedAt    int64            `json:"createdAt"`
	Status       model.RoomStatus `json:"status"`
}

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

type SetRoomSettingReq map[string]any

func (s *SetRoomSettingReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SetRoomSettingReq) Validate() error {
	return nil
}
