package vendorAlist

import (
	"errors"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/vendors/api/alist"
)

type ListReq struct {
	Path     string `json:"path"`
	Password string `json:"password"`
	Refresh  bool   `json:"refresh"`
}

func (r *ListReq) Validate() error {
	if r.Path == "" {
		r.Password = "/"
	}
	return nil
}

func (r *ListReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func List(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.User)

	req := ListReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var cli = vendor.AlistClient(ctx.Query("backend"))

	cacheI, err := user.Cache.LoadOrStoreWithDynamicFunc("alist_authorization", initAlistAuthorizationCacheWithUserID(ctx, cli, user.ID), time.Hour*24)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			ctx.JSON(http.StatusOK, model.NewApiDataResp(&AlistMeResp{
				IsLogin: false,
			}))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	cache, ok := cacheI.(*alistCache)
	if !ok {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	resp, err := cli.FsList(ctx, &alist.FsListReq{
		Token:    cache.Token,
		Password: req.Password,
		Path:     req.Path,
		Host:     cache.Host,
		Refresh:  req.Refresh,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}
