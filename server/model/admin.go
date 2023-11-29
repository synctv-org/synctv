package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	dbModel "github.com/synctv-org/synctv/internal/model"
)

var (
	ErrInvalidID = errors.New("invalid id")
)

type AdminSettingsReq map[string]any

func (asr *AdminSettingsReq) Validate() error {
	return nil
}

func (asr *AdminSettingsReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(asr)
}

type AdminSettingsResp map[dbModel.SettingGroup]map[string]any

type AddUserReq struct {
	Username string       `json:"username"`
	Password string       `json:"password"`
	Role     dbModel.Role `json:"role"`
}

func (aur *AddUserReq) Validate() error {
	if aur.Username == "" {
		return errors.New("username is empty")
	} else if len(aur.Username) > 32 {
		return ErrUsernameTooLong
	} else if !alnumPrintHanReg.MatchString(aur.Username) {
		return ErrUsernameHasInvalidChar
	}

	if aur.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(aur.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(aur.Password) {
		return ErrPasswordHasInvalidChar
	}

	return nil
}

func (aur *AddUserReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(aur)
}

type AdminUserPasswordReq struct {
	ID       string `json:"id"`
	Password string `json:"password"`
}

func (aur *AdminUserPasswordReq) Validate() error {
	if aur.ID == "" {
		return ErrInvalidID
	}

	if aur.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(aur.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(aur.Password) {
		return ErrPasswordHasInvalidChar
	}

	return nil
}

func (aur *AdminUserPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(aur)
}

type AdminUsernameReq struct {
	ID       string `json:"id"`
	Username string `json:"username"`
}

func (aur *AdminUsernameReq) Validate() error {
	if aur.ID == "" {
		return ErrInvalidID
	}

	if aur.Username == "" {
		return errors.New("username is empty")
	} else if len(aur.Username) > 32 {
		return ErrUsernameTooLong
	} else if !alnumPrintHanReg.MatchString(aur.Username) {
		return ErrUsernameHasInvalidChar
	}

	return nil
}

func (aur *AdminUsernameReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(aur)
}

type AdminRoomPasswordReq struct {
	ID       string `json:"id"`
	Password string `json:"password"`
}

func (aur *AdminRoomPasswordReq) Validate() error {
	if aur.ID == "" {
		return ErrInvalidID
	}

	if aur.Password == "" {
		return FormatEmptyPasswordError("room")
	} else if len(aur.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alnumPrintReg.MatchString(aur.Password) {
		return ErrPasswordHasInvalidChar
	}

	return nil
}

func (aur *AdminRoomPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(aur)
}
