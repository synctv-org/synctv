package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
)

func BindApi(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	meta := model.OAuth2Req{}
	if err := model.Decode(ctx, &meta); err != nil {
		log.Errorf("failed to decode request: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	state := utils.RandString(16)
	url, err := pi.NewAuthURL(ctx, state)
	if err != nil {
		log.Errorf("failed to get auth url: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	states.Store(state, newBindFunc(user.ID, meta.Redirect), time.Minute*5)
	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"url": url,
	}))
}

func UnBindApi(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err = db.UnBindProvider(user.ID, pi.Provider())
	if err != nil {
		log.Errorf("failed to unbind provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func newBindFunc(userID, redirect string) stateHandler {
	return func(ctx *gin.Context, pi provider.ProviderInterface, code string) {
		log := ctx.MustGet("log").(*logrus.Entry)

		ui, err := pi.GetUserInfo(ctx, code)
		if err != nil {
			log.Errorf("failed to get user info: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		user, err := op.LoadOrInitUserByID(userID)
		if err != nil {
			log.Errorf("failed to load user: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		err = user.Value().BindProvider(pi.Provider(), ui.ProviderUserID)
		if err != nil {
			log.Errorf("failed to bind provider: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		token, err := middlewares.NewAuthUserToken(user.Value())
		if err != nil {
			log.Errorf("failed to generate token: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}

		ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
			"token":    token,
			"redirect": redirect,
		}))
	}
}
