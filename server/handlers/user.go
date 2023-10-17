package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
)

func Me(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"username": user.Username,
	}))
}

func SetUserPassword(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := model.SetUserPasswordReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SetPassword(req.Password); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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

func LoginUser(ctx *gin.Context) {
	req := model.LoginUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user, err := middlewares.AuthUserWithPassword(req.Username, req.Password)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !user.CheckPassword(req.Password) {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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

func SignupUser(ctx *gin.Context) {
	req := model.SignupUserReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	user, err := op.CreateUser(req.Username, req.Password)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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
