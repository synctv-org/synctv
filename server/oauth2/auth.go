package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"golang.org/x/oauth2"
)

// /oauth2/login/:type
func OAuth2(ctx *gin.Context) {
	t := ctx.Param("type")
	p := provider.OAuth2Provider(t)
	c, ok := conf.Conf.OAuth2[p]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 provider"))
	}

	pi, err := p.GetProvider()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	state := utils.RandString(16)
	states.Store(state, struct{}{}, time.Minute*5)

	RenderRedirect(ctx, pi.NewConfig(c.ClientID, c.ClientSecret).AuthCodeURL(state, oauth2.AccessTypeOnline))
}

func OAuth2Api(ctx *gin.Context) {
	t := ctx.Param("type")
	p := provider.OAuth2Provider(t)
	c, ok := conf.Conf.OAuth2[p]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 provider"))
	}

	pi, err := p.GetProvider()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	state := utils.RandString(16)
	states.Store(state, struct{}{}, time.Minute*5)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"url": pi.NewConfig(c.ClientID, c.ClientSecret).AuthCodeURL(state, oauth2.AccessTypeOnline),
	}))
}

// /oauth2/callback/:type
func OAuth2Callback(ctx *gin.Context) {
	t := ctx.Param("type")
	p := provider.OAuth2Provider(t)
	c, ok := conf.Conf.OAuth2[p]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 provider"))
	}

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

	pi, err := p.GetProvider()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	ui, err := pi.GetUserInfo(ctx, pi.NewConfig(c.ClientID, c.ClientSecret), code)
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
	t := ctx.Param("type")
	p := provider.OAuth2Provider(t)
	c, ok := conf.Conf.OAuth2[p]
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid oauth2 provider"))
	}

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

	pi, err := p.GetProvider()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	ui, err := pi.GetUserInfo(ctx, pi.NewConfig(c.ClientID, c.ClientSecret), req.Code)
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
