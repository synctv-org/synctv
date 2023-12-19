package vendorEmby

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
	"github.com/synctv-org/vendors/api/emby"
)

type LoginReq struct {
	Host     string `json:"host"`
	Username string `json:"username"`
	Password string `json:"password"`
	ApiKey   string `json:"apiKey"`
}

func (r *LoginReq) Validate() error {
	if r.Host == "" {
		return errors.New("host is required")
	}
	if r.ApiKey == "" && (r.Username == "" || r.Password == "") {
		return errors.New("username and password or apiKey is required")
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

	backend := ctx.Query("backend")
	cli := vendor.LoadEmbyClient(backend)

	if req.ApiKey != "" {
		_, err := cli.GetSystemInfo(ctx, &emby.SystemInfoReq{
			Host:  req.Host,
			Token: req.ApiKey,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	} else {
		data, err := cli.Login(ctx, &emby.LoginReq{
			Host:     req.Host,
			Username: req.Username,
			Password: req.Password,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		req.ApiKey = data.Token
	}

	_, err := db.CreateOrSaveEmbyVendor(user.ID, &dbModel.EmbyVendor{
		UserID:  user.ID,
		Host:    req.Host,
		ApiKey:  req.ApiKey,
		Backend: backend,
	})

	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	_, err = user.EmbyCache().Data().Refresh(ctx, func(ctx context.Context, args ...struct{}) (*cache.EmbyUserCacheData, error) {
		return &cache.EmbyUserCacheData{
			Host:    req.Host,
			ApiKey:  req.ApiKey,
			Backend: backend,
		}, nil
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func Logout(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	err := db.DeleteEmbyVendor(user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}
