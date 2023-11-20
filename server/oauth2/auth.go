package auth

import (
	"context"
	"errors"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
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
	states.Store(state, stateMeta{
		OAuth2Req: model.OAuth2Req{
			Redirect: ctx.Query("redirect"),
		},
	}, time.Minute*5)

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
	states.Store(state, stateMeta{
		OAuth2Req: meta,
	}, time.Minute*5)

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

	ld, err := login(ctx, ctx.Query("state"), code, pi)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	RenderToken(ctx, ld.redirect, ld.token)
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

	ld, err := login(ctx, req.State, req.Code, pi)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token":    ld.token,
		"redirect": ld.redirect,
	}))
}

type loginData struct {
	token, redirect string
}

func login(ctx context.Context, state, code string, pi provider.ProviderInterface) (*loginData, error) {
	meta, loaded := states.LoadAndDelete(state)
	if !loaded {
		return nil, errors.New("invalid oauth2 state")
	}

	t, err := pi.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}

	ui, err := pi.GetUserInfo(ctx, t)
	if err != nil {
		return nil, err
	}

	var user *op.User
	if meta.Value().BindUserId != "" {
		user, err = op.GetUserById(meta.Value().BindUserId)
	} else if settings.DisableUserSignup.Get() {
		user, err = op.GetUserByProvider(pi.Provider(), ui.ProviderUserID)
	} else {
		if settings.SignupNeedReview.Get() {
			user, err = op.CreateOrLoadUser(ui.Username, pi.Provider(), ui.ProviderUserID, db.WithRole(dbModel.RolePending))
		} else {
			user, err = op.CreateOrLoadUser(ui.Username, pi.Provider(), ui.ProviderUserID)
		}
	}
	if err != nil {
		return nil, err
	}

	if meta.Value().BindUserId != "" {
		err = op.BindProvider(meta.Value().BindUserId, pi.Provider(), ui.ProviderUserID)
		if err != nil {
			return nil, err
		}
	}

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		return nil, err
	}

	redirect := "/web/"
	if meta.Value().Redirect != "" {
		redirect = meta.Value().Redirect
	}

	return &loginData{
		token:    token,
		redirect: redirect,
	}, nil
}
