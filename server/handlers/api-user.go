package handlers

import (
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/room"
)

func Me(ctx *gin.Context) {
	rooms := ctx.Value("rooms").(*room.Rooms)
	user, err := AuthRoom(ctx.GetHeader("Authorization"), rooms)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"isRoot":   user.IsRoot(),
		"isAdmin":  user.IsAdmin(),
		"username": user.Name(),
		"lastAct":  user.LastAct(),
	}))
}

func SetUserPassword(ctx *gin.Context) {
	rooms := ctx.Value("rooms").(*room.Rooms)
	user, err := AuthRoom(ctx.GetHeader("Authorization"), rooms)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(SetPasswordReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.SetPassword(req.Password); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	user.CloseHub()

	token, err := newAuthorization(user)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"token": token,
	}))
}
