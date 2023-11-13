package handlers

import (
	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/server/model"
)

func Settings(ctx *gin.Context) {
	ctx.JSON(200, model.NewApiDataResp(gin.H{}))
}
