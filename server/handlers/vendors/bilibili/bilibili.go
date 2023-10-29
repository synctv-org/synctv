package Vbilibili

import (
	"errors"
	"net/http"
	"strconv"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/vendors/bilibili"
)

func QRCode(ctx *gin.Context) {
	r, err := bilibili.NewQRCode()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	ctx.JSON(http.StatusOK, model.NewApiDataResp(r))
}

type LoginReq struct {
	Key string `json:"key"`
}

func (r *LoginReq) Validate() error {
	if r.Key == "" {
		return errors.New("key is empty")
	}
	return nil
}

func (r *LoginReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func Login(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := LoginReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	cookie, err := bilibili.Login(req.Key)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	_, err = db.AssignFirstOrCreateVendorByUserIDAndVendor(user.ID, dbModel.StreamingVendorBilibili, db.WithCookie([]*http.Cookie{cookie}))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type ParseReq struct {
	URL string `json:"url"`
}

func (r *ParseReq) Validate() error {
	if r.URL == "" {
		return errors.New("url is empty")
	}
	return nil
}

func (r *ParseReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func Parse(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := ParseReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	matchType, id, err := bilibili.Match(req.URL)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	vendor, err := db.FirstOrCreateVendorByUserIDAndVendor(user.ID, dbModel.StreamingVendorBilibili)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	cli := bilibili.NewClient(vendor.Cookies)

	switch matchType {
	case "bv":
		mpis, err := cli.ParseVideoPage(0, id)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(mpis))
	case "av":
		aid, err := strconv.Atoi(id)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		mpis, err := cli.ParseVideoPage(uint(aid), "")
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(mpis))
	case "ep":
		epId, err := strconv.Atoi(id)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		mpis, err := cli.ParsePGCPage(uint(epId), 0)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(mpis))
	case "ss":
		seasonId, err := strconv.Atoi(id)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		mpis, err := cli.ParsePGCPage(0, uint(seasonId))
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.JSON(http.StatusOK, model.NewApiDataResp(mpis))
	default:
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("unknown match type"))
		return
	}
}
