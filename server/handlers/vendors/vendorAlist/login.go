package vendorAlist

import (
	"context"
	"errors"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
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

type alistCache struct {
	Host  string
	Token string
}

func initAlistAuthorizationCacheWithConfig(ctx context.Context, cli alist.AlistHTTPServer, host, username, password string) func() (any, error) {
	return func() (any, error) {
		if username == "" {
			_, err := cli.Me(ctx, &alist.MeReq{
				Host: host,
			})
			return &alistCache{
				Host: host,
			}, err
		} else {
			resp, err := cli.Login(ctx, &alist.LoginReq{
				Host:     host,
				Username: username,
				Password: password,
			})
			if err != nil {
				return nil, err
			}
			return &alistCache{
				Host:  host,
				Token: resp.Token,
			}, nil
		}
	}
}

func initAlistAuthorizationCacheWithUserID(ctx context.Context, cli alist.AlistHTTPServer, userID string) func() (any, error) {
	return func() (any, error) {
		v, err := db.GetAlistVendor(userID)
		if err != nil {
			return nil, err
		}

		return initAlistAuthorizationCacheWithConfig(ctx, cli, v.Host, v.Username, v.Password)()
	}
}

func Login(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := LoginReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	cli := vendor.AlistClient("")

	var (
		authI any
		err   error
	)
	if req.Username == "" {
		_, err = cli.Me(ctx, &alist.MeReq{
			Host: req.Host,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		}

		authI, err = user.Cache.StoreOrRefreshWithDynamicFunc("alist_authorization", func() (any, error) {
			return &alistCache{
				Host: req.Host,
			}, nil
		}, time.Hour*24)
	} else {
		var resp *alist.LoginResp
		resp, err = cli.Login(ctx, &alist.LoginReq{
			Host:     req.Host,
			Username: req.Username,
			Password: req.Password,
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		authI, err = user.Cache.StoreOrRefreshWithDynamicFunc("alist_authorization", func() (any, error) {
			return &alistCache{
				Host:  req.Host,
				Token: resp.Token,
			}, nil
		}, time.Hour*24)
	}
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	_, ok := authI.(*alistCache)
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	_, err = db.CreateOrSaveAlistVendor(user.ID, &dbModel.AlistVendor{
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
