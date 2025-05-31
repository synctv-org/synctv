package handlers

import (
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/email"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
)

type publicSettings struct {
	EmailWhitelist        []string `json:"emailWhitelist,omitempty"`
	PasswordDisableSignup bool     `json:"passwordDisableSignup"`
	EmailEnable           bool     `json:"emailEnable"`
	EmailDisableSignup    bool     `json:"emailDisableSignup"`
	EmailWhitelistEnabled bool     `json:"emailWhitelistEnabled"`
	Oauth2DisableSignup   bool     `json:"oauth2DisableSignup"`
	GuestEnable           bool     `json:"guestEnable"`
	P2PZone               string   `json:"p2pZone"`
}

func Settings(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	oauth2SignupEnabled, err := bootstrap.Oauth2SignupEnabledCache.Get(ctx)
	if err != nil {
		log.Errorf("failed to get oauth2 signup enabled: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	ctx.JSON(200, model.NewAPIDataResp(
		&publicSettings{
			PasswordDisableSignup: settings.DisableUserSignup.Get() ||
				!settings.EnablePasswordSignup.Get(),

			EmailEnable: email.EnableEmail.Get(),
			EmailDisableSignup: settings.DisableUserSignup.Get() ||
				email.DisableUserSignup.Get(),
			EmailWhitelistEnabled: email.EmailSignupWhiteListEnable.Get(),
			EmailWhitelist:        strings.Split(email.EmailSignupWhiteList.Get(), ","),

			Oauth2DisableSignup: settings.DisableUserSignup.Get() || len(oauth2SignupEnabled) == 0,

			GuestEnable: settings.EnableGuest.Get(),
			P2PZone:     settings.P2PZone.Get(),
		},
	))
}
