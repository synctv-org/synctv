package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
)

type SetUserPasswordReq struct {
	Password string `json:"password"`
}

func (s *SetUserPasswordReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SetUserPasswordReq) Validate() error {
	if s.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(s.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alphaNumReg.MatchString(s.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}

type LoginUserReq struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

func (l *LoginUserReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(l)
}

func (l *LoginUserReq) Validate() error {
	if l.Username == "" {
		return errors.New("username is empty")
	} else if len(l.Username) > 32 {
		return ErrUsernameTooLong
	}

	if l.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(l.Password) > 32 {
		return ErrPasswordTooLong
	}
	return nil
}

type SignupUserReq struct {
	Username string `json:"username"`
	Password string `json:"password"`
}

func (s *SignupUserReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(s)
}

func (s *SignupUserReq) Validate() error {
	if s.Username == "" {
		return errors.New("username is empty")
	} else if len(s.Username) > 32 {
		return ErrUsernameTooLong
	} else if !alphaNumChineseReg.MatchString(s.Username) {
		return ErrUsernameHasInvalidChar
	}

	if s.Password == "" {
		return FormatEmptyPasswordError("user")
	} else if len(s.Password) > 32 {
		return ErrPasswordTooLong
	} else if !alphaNumReg.MatchString(s.Password) {
		return ErrPasswordHasInvalidChar
	}
	return nil
}
