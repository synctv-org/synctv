package vendoralist

import (
	"context"
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"net/http"
	"net/url"
	"strings"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/server/model"
)

type LoginReq struct {
	Host           string `json:"host"`
	Username       string `json:"username"`
	Password       string `json:"password"`
	HashedPassword string `json:"hashedPassword"`
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
	if r.Password != "" && r.HashedPassword != "" {
		return errors.New("password and hashedPassword can't be both set")
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	if req.Password != "" {
		h := sha256.New()
		h.Write([]byte(req.Password + `-https://github.com/alist-org/alist`))
		req.HashedPassword = hex.EncodeToString(h.Sum(nil))
	}

	backend := ctx.Query("backend")

	data, err := cache.AlistAuthorizationCacheWithConfigInitFunc(ctx, &dbModel.AlistVendor{
		Host:           req.Host,
		Username:       req.Username,
		HashedPassword: []byte(req.HashedPassword),
		Backend:        backend,
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	_, err = db.CreateOrSaveAlistVendor(&dbModel.AlistVendor{
		UserID:         user.ID,
		ServerID:       data.ServerID,
		Backend:        data.Backend,
		Host:           data.Host,
		Username:       req.Username,
		HashedPassword: []byte(req.HashedPassword),
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	_, err = user.AlistCache().StoreOrRefreshWithDynamicFunc(ctx, data.ServerID, func(ctx context.Context, key string, args ...struct{}) (*cache.AlistUserCacheData, error) {
		return data, nil
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func Logout(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	var req model.ServerIDReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	err := db.DeleteAlistVendor(user.ID, req.ServerID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	if rc, ok := user.AlistCache().LoadCache(req.ServerID); ok {
		err = rc.Clear(ctx)
		if err != nil {
			log.Errorf("clear alist cache error: %v", err)
		}
	}

	ctx.Status(http.StatusNoContent)
}
