package auth

import (
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
)

func BindApi(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

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
		OAuth2Req:  meta,
		BindUserId: user.ID,
	}, time.Minute*5)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"url": pi.NewAuthURL(state),
	}))
}

func UnBindApi(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	pi, err := providers.GetProvider(provider.OAuth2Provider(ctx.Param("type")))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err = db.UnBindProvider(user.ID, pi.Provider())
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
