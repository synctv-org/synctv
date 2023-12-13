package vendorAlist

import (
	"context"
	"errors"
	"net/http"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/alist"
)

type LoginReq struct {
	Host     string `json:"host"`
	Username string `json:"username"`
	Password string `json:"password"`
}

func (r *LoginReq) Validate() error {
	if r.Host == "" {
		return errors.New("host is required")
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

	cli := vendor.AlistClient("")

	if req.Username == "" {
		_, err := cli.Me(ctx, &alist.MeReq{
			Host: req.Host,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		_, err = user.AlistCache().Data().Refresh(ctx, func(ctx context.Context, args ...string) (*cache.AlistUserCacheData, error) {
			return &cache.AlistUserCacheData{
				Host: req.Host,
			}, nil
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
	} else {
		resp, err := cli.Login(ctx, &alist.LoginReq{
			Host:     req.Host,
			Username: req.Username,
			Password: req.Password,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		_, err = user.AlistCache().Data().Refresh(ctx, func(ctx context.Context, args ...string) (*cache.AlistUserCacheData, error) {
			return &cache.AlistUserCacheData{
				Host:  req.Host,
				Token: resp.Token,
			}, nil
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
	}

	_, err := db.CreateOrSaveAlistVendor(user.ID, &dbModel.AlistVendor{
		Host:     req.Host,
		Username: req.Username,
		Password: req.Password,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
