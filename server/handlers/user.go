package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
)

func Me(ctx *gin.Context) {
	user := ctx.Value("user").(*room.User)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"isRoot":   user.IsRoot(),
		"isAdmin":  user.IsAdmin(),
		"username": user.Name(),
		"lastAct":  user.LastAct(),
	}))
}

func SetUserPassword(ctx *gin.Context) {
	user := ctx.Value("user").(*room.User)

	req := new(SetPasswordReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetPassword(req.Password); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user.CloseHub()

	token, err := middlewares.NewAuthToken(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"token": token,
	}))
}
