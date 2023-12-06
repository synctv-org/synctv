package auth

import (
	"fmt"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
)

// GET
// /oauth2/login/:type
func OAuth2(ctx *gin.Context) {
	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	state := utils.RandString(16)
	states.Store(state, newAuthFunc(ctx.Query("redirect")), time.Minute*5)

	RenderRedirect(ctx, pi.NewAuthURL(state))
}

// POST
func OAuth2Api(ctx *gin.Context) {
	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	meta := model.OAuth2Req{}
	if err := model.Decode(ctx, &meta); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	state := utils.RandString(16)
	states.Store(state, newAuthFunc(meta.Redirect), time.Minute*5)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"url": pi.NewAuthURL(state),
	}))
}

// GET
// /oauth2/callback/:type
func OAuth2Callback(ctx *gin.Context) {
	code := ctx.Query("code")
	if code == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 code"))
		return
	}

	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	meta, loaded := states.LoadAndDelete(ctx.Query("state"))
	if !loaded {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 state"))
		return
	}

	if meta.Value() != nil {
		meta.Value()(ctx, pi, code)
	}
}

// POST
// /oauth2/callback/:type
func OAuth2CallbackApi(ctx *gin.Context) {
	req := model.OAuth2CallbackReq{}
	if err := req.Decode(ctx); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	meta, loaded := states.LoadAndDelete(req.State)
	if !loaded {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 state"))
		return
	}

	if meta.Value() != nil {
		meta.Value()(ctx, pi, req.Code)
	}
}

func newAuthFunc(redirect string) stateHandler {
	return func(ctx *gin.Context, pi provider.ProviderInterface, code string) {
		t, err := pi.GetToken(ctx, code)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		ui, err := pi.GetUserInfo(ctx, t)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		pgs, loaded := bootstrap.ProviderGroupSettings[dbModel.SettingGroup(fmt.Sprintf("%s_%s", dbModel.SettingGroupOauth2, pi.Provider()))]
		if !loaded {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 provider"))
			return
		}

		var user *op.User
		if settings.DisableUserSignup.Get() || pgs.DisableUserSignup.Get() {
			user, err = op.GetUserByProvider(pi.Provider(), ui.ProviderUserID)
		} else {
			if settings.SignupNeedReview.Get() || pgs.SignupNeedReview.Get() {
				user, err = op.CreateOrLoadUserWithProvider(ui.Username, utils.RandString(16), pi.Provider(), ui.ProviderUserID, db.WithRole(dbModel.RolePending))
			} else {
				user, err = op.CreateOrLoadUserWithProvider(ui.Username, utils.RandString(16), pi.Provider(), ui.ProviderUserID)
			}
		}
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		token, err := middlewares.NewAuthUserToken(user)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		if ctx.Request.Method == http.MethodGet {
			RenderToken(ctx, redirect, token)
		} else if ctx.Request.Method == http.MethodPost {
			ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
				"token":    token,
				"redirect": redirect,
			}))
		}
	}
}
