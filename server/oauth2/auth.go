package auth

import (
	"errors"
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
	log := middlewares.GetLogger(ctx)

	pi, err := providers.GetProvider(ctx.Param("type"))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	state := utils.RandString(16)
	url, err := pi.NewAuthURL(ctx, state)
	if err != nil {
		log.Errorf("failed to get auth url: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}
	states.Store(state, newAuthFunc(ctx.Query("redirect")), time.Minute*5)

	err = RenderRedirect(ctx, url)
	if err != nil {
		log.Errorf("failed to render redirect: %v", err)
	}
}

// POST
func OAuth2Api(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	pi, err := providers.GetProvider(ctx.Param("type"))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
	}

	meta := model.OAuth2Req{}
	if err := model.Decode(ctx, &meta); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	state := utils.RandString(16)
	url, err := pi.NewAuthURL(ctx, state)
	if err != nil {
		log.Errorf("failed to get auth url: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}
	states.Store(state, newAuthFunc(meta.Redirect), time.Minute*5)
	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"url": url,
	}))
}

// GET
// /oauth2/callback/:type
func OAuth2Callback(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	code := ctx.Query("code")
	if code == "" {
		log.Errorf("invalid oauth2 code")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("invalid oauth2 code"),
		)
		return
	}

	pi, err := providers.GetProvider(ctx.Param("type"))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	meta, loaded := states.LoadAndDelete(ctx.Query("state"))
	if !loaded {
		log.Errorf("invalid oauth2 state")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("invalid oauth2 state"),
		)
		return
	}

	if meta.Value() != nil {
		meta.Value()(ctx, pi, code)
	} else {
		log.Errorf("invalid oauth2 handler")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorStringResp("invalid oauth2 handler"))
	}
}

// POST
// /oauth2/callback/:type
func OAuth2CallbackAPI(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	req := model.OAuth2CallbackReq{}
	if err := req.Decode(ctx); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	pi, err := providers.GetProvider(ctx.Param("type"))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
	}

	meta, loaded := states.LoadAndDelete(req.State)
	if !loaded {
		log.Errorf("invalid oauth2 state")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("invalid oauth2 state"),
		)
		return
	}

	if meta.Value() != nil {
		meta.Value()(ctx, pi, req.Code)
	} else {
		log.Errorf("invalid oauth2 handler")
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorStringResp("invalid oauth2 handler"))
	}
}

func newAuthFunc(redirect string) stateHandler {
	return func(ctx *gin.Context, pi provider.Interface, code string) {
		log := middlewares.GetLogger(ctx)

		ctx.Header("X-OAuth2-Type", CallbackTypeAuth)

		ui, err := pi.GetUserInfo(ctx, code)
		if err != nil {
			log.Errorf("failed to get user info: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		if ui.ProviderUserID == "" {
			log.Errorf("invalid oauth2 provider user id")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("invalid oauth2 provider user id"),
			)
			return
		}
		if ui.Username == "" {
			log.Errorf("invalid oauth2 username")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("invalid oauth2 username"),
			)
			return
		}

		pgs, loaded := bootstrap.ProviderGroupSettings[fmt.Sprintf("%s_%s", dbModel.SettingGroupOauth2, pi.Provider())]
		if !loaded {
			log.Errorf("invalid oauth2 provider")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("invalid oauth2 provider"),
			)
			return
		}

		var userE *op.UserEntry
		if settings.DisableUserSignup.Get() || pgs.DisableUserSignup.Get() {
			userE, err = op.GetUserByProvider(pi.Provider(), ui.ProviderUserID)
		} else {
			if settings.SignupNeedReview.Get() || pgs.SignupNeedReview.Get() {
				userE, err = op.CreateOrLoadUserWithProvider(ui.Username, utils.RandString(16), pi.Provider(), ui.ProviderUserID, db.WithRole(dbModel.RolePending))
			} else {
				userE, err = op.CreateOrLoadUserWithProvider(ui.Username, utils.RandString(16), pi.Provider(), ui.ProviderUserID)
			}
		}
		if err != nil {
			log.Errorf("failed to create or load user: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}
		user := userE.Value()

		token, err := middlewares.NewAuthUserToken(user)
		if err != nil {
			if errors.Is(err, middlewares.ErrUserBanned) ||
				errors.Is(err, middlewares.ErrUserPending) {
				ctx.AbortWithStatusJSON(http.StatusOK, model.NewAPIDataResp(gin.H{
					"type":    CallbackTypeAuth,
					"message": err.Error(),
					"role":    user.Role,
				}))
				return
			}
			log.Errorf("failed to generate token: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		switch ctx.Request.Method {
		case http.MethodGet:
			err = RenderToken(ctx, redirect, token)
			if err != nil {
				log.Errorf("failed to render token: %v", err)
			}
		case http.MethodPost:
			ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
				"type":     CallbackTypeAuth,
				"role":     user.Role,
				"token":    token,
				"redirect": redirect,
			}))
		}
	}
}
