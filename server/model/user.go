package model

import (
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
