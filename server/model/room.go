package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	dbModel "github.com/synctv-org/synctv/internal/model"
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
	return string(f) + " password empty"
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
	switch {
	case c.RoomName == "":
		return ErrEmptyRoomName
	case len(c.RoomName) > 32:
		return ErrRoomNameTooLong
	case !alnumPrintHanReg.MatchString(c.RoomName):
		return ErrRoomNameHasInvalidChar
	}

	if c.Password != "" {
		switch {
		case len(c.Password) > 32:
			return ErrPasswordTooLong
		case !alnumPrintReg.MatchString(c.Password):
			return ErrPasswordHasInvalidChar
		}
	}

	return nil
}

type RoomListResp struct {
	RoomID       string             `json:"roomId"`
	RoomName     string             `json:"roomName"`
	CreatorID    string             `json:"creatorId"`
	Creator      string             `json:"creator"`
	ViewerCount  int64              `json:"viewerCount"`
	CreatedAt    int64              `json:"createdAt"`
	NeedPassword bool               `json:"needPassword"`
	Status       dbModel.RoomStatus `json:"status"`
}

type JoinedRoomResp struct {
	RoomListResp
	MemberStatus dbModel.RoomMemberStatus `json:"memberStatus"`
	MemberRole   dbModel.RoomMemberRole   `json:"memberRole"`
}

type LoginRoomReq struct {
	RoomID   string `json:"roomId"`
	Password string `json:"password"`
}

func (l *LoginRoomReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginRoomReq) Validate() error {
	if l.RoomID == "" {
		return ErrEmptyRoomName
	} else if len(l.RoomID) != 32 {
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
	ID string `json:"id"`
}

func (r *RoomIDReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func (r *RoomIDReq) Validate() error {
	if len(r.ID) != 32 {
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

type CheckRoomPasswordReq struct {
	Password string `json:"password"`
}

func (c *CheckRoomPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(c)
}

func (c *CheckRoomPasswordReq) Validate() error {
	return nil
}

type CheckRoomResp struct {
	Name         string             `json:"name"`
	CreatorID    string             `json:"creatorId"`
	Creator      string             `json:"creator"`
	ViewerCount  int64              `json:"viewerCount"`
	Status       dbModel.RoomStatus `json:"status"`
	NeedPassword bool               `json:"needPassword"`
	EnabledGuest bool               `json:"enabledGuest"`
}
