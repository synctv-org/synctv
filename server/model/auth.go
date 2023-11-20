package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
)

type OAuth2CallbackReq struct {
	Code  string `json:"code"`
	State string `json:"state"`
}

var (
	ErrInvalidOAuth2Code  = errors.New("invalid oauth2 code")
	ErrInvalidOAuth2State = errors.New("invalid oauth2 state")
)

func (o *OAuth2CallbackReq) Validate() error {
	if o.Code == "" {
		return ErrInvalidOAuth2Code
	}
	if o.State == "" {
		return ErrInvalidOAuth2State
	}
	return nil
}

func (o *OAuth2CallbackReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(o)
}

type OAuth2Req struct {
	Redirect string `json:"redirect"`
}

func (o *OAuth2Req) Validate() error {
	return nil
}

func (o *OAuth2Req) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(o)
}
