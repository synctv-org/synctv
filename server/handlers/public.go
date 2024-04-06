package handlers

import (
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/email"
	"github.com/synctv-org/synctv/server/model"
)

type publicSettings struct {
	EmailWhitelistEnabled bool     `json:"emailWhitelistEnabled"`
	EmailWhitelist        []string `json:"emailWhitelist,omitempty"`
}

func Settings(ctx *gin.Context) {
	ctx.JSON(200, model.NewApiDataResp(
		&publicSettings{
			EmailWhitelistEnabled: email.EmailSignupWhiteListEnable.Get(),
			EmailWhitelist:        strings.Split(email.EmailSignupWhiteList.Get(), ","),
		},
	))
}
