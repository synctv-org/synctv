package handlers

import (
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/email"
	"github.com/synctv-org/synctv/server/model"
)

type publicSettings struct {
	EmailEnable            bool     `json:"emailEnable"`
	EmailDisableUserSignup bool     `json:"EmailDisableUserSignup"`
	EmailWhitelistEnabled  bool     `json:"emailWhitelistEnabled"`
	EmailWhitelist         []string `json:"emailWhitelist,omitempty"`
}

func Settings(ctx *gin.Context) {
	ctx.JSON(200, model.NewApiDataResp(
		&publicSettings{
			EmailEnable:            email.EnableEmail.Get(),
			EmailDisableUserSignup: email.DisableUserSignup.Get(),
			EmailWhitelistEnabled:  email.EmailSignupWhiteListEnable.Get(),
			EmailWhitelist:         strings.Split(email.EmailSignupWhiteList.Get(), ","),
		},
	))
}
