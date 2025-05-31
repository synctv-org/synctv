package handlers

import (
	"context"
	"net/http"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/server/handlers/vendors"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
)

func StreamDanmu(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	room := middlewares.GetRoomEntry(ctx).Value()
	// user := middlewares.GetUserEntry(ctx).Value()

	m, err := room.GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		log.Errorf("get movie by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	v, err := vendors.NewVendorService(room, m)
	if err != nil {
		log.Errorf("new vendor service error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	danmu, ok := v.(vendors.VendorDanmuService)
	if !ok {
		log.Errorf("vendor %s not support danmu", m.VendorInfo.Vendor)
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("vendor not support danmu"),
		)
		return
	}

	c, cancel := context.WithCancel(ctx.Request.Context())
	defer cancel()

	err = danmu.StreamDanmu(c, func(danmu string) error {
		ctx.SSEvent("danmu", danmu)
		if err := ctx.Err(); err != nil {
			return err
		}
		ctx.Writer.Flush()
		return nil
	})
	if err != nil {
		log.Errorf("stream danmu error: %v", err)
		ctx.SSEvent("error", err.Error())
	}
}
