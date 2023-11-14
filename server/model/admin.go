package model

import (
	"errors"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
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

type AdminSettingsResp map[string]map[string]any
