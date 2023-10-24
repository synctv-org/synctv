package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
)

// /oauth2/login/:type
func OAuth2(ctx *gin.Context) {
	t := ctx.Param("type")

	pi, err := providers.GetProvider(provider.OAuth2Provider(t))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	state := utils.RandString(16)
	states.Store(state, struct{}{}, time.Minute*5)

	RenderRedirect(ctx, pi.NewAuthURL(state))
}

func OAuth2Api(ctx *gin.Context) {
	t := ctx.Param("type")
	pi, err := providers.GetProvider(provider.OAuth2Provider(t))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	state := utils.RandString(16)
	states.Store(state, struct{}{}, time.Minute*5)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"url": pi.NewAuthURL(state),
	}))
}

// /oauth2/callback/:type
func OAuth2Callback(ctx *gin.Context) {
	code := ctx.Query("code")
	if code == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 code"))
		return
	}

	state := ctx.Query("state")
	if state == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 state"))
		return
	}

	_, loaded := states.LoadAndDelete(state)
	if !loaded {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 state"))
		return
	}

	p := provider.OAuth2Provider(ctx.Param("type"))
	pi, err := providers.GetProvider(p)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

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

	user, err := op.CreateOrLoadUser(ui.Username, p, ui.ProviderUserID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	RenderToken(ctx, "/web/", token)
}

// /oauth2/callback/:type
func OAuth2CallbackApi(ctx *gin.Context) {
	req := model.OAuth2CallbackReq{}
	if err := req.Decode(ctx); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	_, loaded := states.LoadAndDelete(req.State)
	if !loaded {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 state"))
		return
	}

	p := provider.OAuth2Provider(ctx.Param("type"))
	pi, err := providers.GetProvider(p)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	t, err := pi.GetToken(ctx, req.Code)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ui, err := pi.GetUserInfo(ctx, t)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user, err := op.CreateOrLoadUser(ui.Username, p, ui.ProviderUserID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	token, err := middlewares.NewAuthUserToken(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}
