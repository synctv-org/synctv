package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
)

func BindAPI(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()

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

	states.Store(state, newBindFunc(user.ID, meta.Redirect), time.Minute*5)
	ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
		"url": url,
	}))
}

func UnBindAPI(ctx *gin.Context) {
	user := middlewares.GetUserEntry(ctx).Value()
	log := middlewares.GetLogger(ctx)

	pi, err := providers.GetProvider(ctx.Param("type"))
	if err != nil {
		log.Errorf("failed to get provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err = db.UnBindProvider(user.ID, pi.Provider())
	if err != nil {
		log.Errorf("failed to unbind provider: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func newBindFunc(userID, redirect string) stateHandler {
	return func(ctx *gin.Context, pi provider.Interface, code string) {
		log := middlewares.GetLogger(ctx)

		ctx.Header("X-OAuth2-Type", CallbackTypeBind)

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

		user, err := op.LoadOrInitUserByID(userID)
		if err != nil {
			log.Errorf("failed to load user: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		err = user.Value().BindProvider(pi.Provider(), ui.ProviderUserID)
		if err != nil {
			log.Errorf("failed to bind provider: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		ctx.JSON(http.StatusOK, model.NewAPIDataResp(gin.H{
			"type":     CallbackTypeBind,
			"redirect": redirect,
		}))
	}
}
