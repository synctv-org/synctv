package handlers

import (
	"context"
	"net/http"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/handlers/vendors"
	"github.com/synctv-org/synctv/server/model"
)

func StreamDanmu(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	// user := ctx.MustGet("user").(*op.UserEntry).Value()

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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("vendor not support danmu"))
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
