package vendorEmby

import (
	"context"
	"errors"
	"net/http"
	"net/url"
	"strings"

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
}

func (r *LoginReq) Validate() error {
	if r.Host == "" {
		return errors.New("host is required")
	}
	url, err := url.Parse(r.Host)
	if err != nil {
		return err
	}
	if url.Scheme != "http" && url.Scheme != "https" {
		return errors.New("host is invalid")
	}
	r.Host = strings.TrimRight(url.String(), "/")
	if r.Username == "" || r.Password == "" {
		return errors.New("username and password is required")
	}
	return nil
}

func (r *LoginReq) Decode(ctx *gin.Context) error {
	return json.NewDecoder(ctx.Request.Body).Decode(r)
}

func Login(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := LoginReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	backend := ctx.Query("backend")
	cli := vendor.LoadEmbyClient(backend)

	data, err := cli.Login(ctx, &emby.LoginReq{
		Host:     req.Host,
		Username: req.Username,
		Password: req.Password,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if data.ServerId == "" {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("serverID is empty"))
		return
	}

	_, err = db.CreateOrSaveEmbyVendor(&dbModel.EmbyVendor{
		UserID:     user.ID,
		ServerID:   data.ServerId,
		Host:       req.Host,
		ApiKey:     data.Token,
		Backend:    backend,
		EmbyUserID: data.UserId,
	})

	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	_, err = user.EmbyCache().StoreOrRefreshWithDynamicFunc(ctx, data.ServerId, func(ctx context.Context, key string, args ...struct{}) (*cache.EmbyUserCacheData, error) {
		return &cache.EmbyUserCacheData{
			Host:     req.Host,
			ServerID: key,
			ApiKey:   data.Token,
			Backend:  backend,
			UserID:   data.UserId,
		}, nil
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func Logout(ctx *gin.Context) {
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	var req model.ServerIDReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := db.DeleteEmbyVendor(user.ID, req.ServerID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	eucd, ok := user.EmbyCache().LoadCache(req.ServerID)
	if ok {
		eucdr, _ := eucd.Raw()
		go logoutEmby(eucdr)
	}

	ctx.Status(http.StatusNoContent)
}

func logoutEmby(eucd *cache.EmbyUserCacheData) {
	if eucd == nil || eucd.ApiKey == "" {
		return
	}
	_, _ = vendor.LoadEmbyClient(eucd.Backend).Logout(context.Background(), &emby.LogoutReq{
		Host:  eucd.Host,
		Token: eucd.ApiKey,
	})
}
