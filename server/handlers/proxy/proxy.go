package proxy

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"
	"sync"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/go-uhc"
)

var (
	defaultCache  Cache = NewMemoryCache()
	fileCacheOnce sync.Once
	fileCache     Cache
)

func getCache() Cache {
	fileCacheOnce.Do(func() {
		if conf.Conf.Server.ProxyCachePath == "" {
			return
		}
		log.Infof("proxy cache path: %s", conf.Conf.Server.ProxyCachePath)
		fileCache = NewFileCache(conf.Conf.Server.ProxyCachePath)
	})
	if fileCache != nil {
		return fileCache
	}
	return defaultCache
}

func ProxyURL(ctx *gin.Context, u string, headers map[string]string, cache bool) error {
	if !settings.AllowProxyToLocal.Get() {
		if l, err := utils.ParseURLIsLocalIP(u); err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest,
				model.NewApiErrorStringResp(
					fmt.Sprintf("check url is local ip error: %v", err),
				),
			)
			return fmt.Errorf("check url is local ip error: %w", err)
		} else if l {
			ctx.AbortWithStatusJSON(http.StatusBadRequest,
				model.NewApiErrorStringResp(
					"not allow proxy to local",
				),
			)
			return errors.New("not allow proxy to local")
		}
	}

	if cache && settings.ProxyCacheEnable.Get() {
		rsc := NewHttpReadSeekCloser(u,
			WithHeadersMap(headers),
			WithNotSupportRange(ctx.GetHeader("Range") == ""),
		)
		defer rsc.Close()
		return NewSliceCacheProxy(u, 1024*512, rsc, getCache()).
			Proxy(ctx.Writer, ctx.Request)
	}

	ctx2, cf := context.WithCancel(ctx)
	defer cf()
	req, err := http.NewRequestWithContext(ctx2, http.MethodGet, u, nil)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewApiErrorStringResp(
				fmt.Sprintf("new request error: %v", err),
			),
		)
		return fmt.Errorf("new request error: %w", err)
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	if r := ctx.GetHeader("Range"); r != "" {
		req.Header.Set("Range", r)
	}
	if r := ctx.GetHeader("Accept-Encoding"); r != "" {
		req.Header.Set("Accept-Encoding", r)
	}
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	cli := http.Client{
		Transport: uhc.DefaultTransport,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			req.Header.Del("Referer")
			for k, v := range headers {
				req.Header.Set(k, v)
			}
			if r := ctx.GetHeader("Range"); r != "" {
				req.Header.Set("Range", r)
			}
			if r := ctx.GetHeader("Accept-Encoding"); r != "" {
				req.Header.Set("Accept-Encoding", r)
			}
			if req.Header.Get("User-Agent") == "" {
				req.Header.Set("User-Agent", utils.UA)
			}
			return nil
		},
	}
	resp, err := cli.Do(req)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewApiErrorStringResp(
				fmt.Sprintf("request url error: %v", err),
			),
		)
		return fmt.Errorf("request url error: %w", err)
	}
	defer resp.Body.Close()
	ctx.Status(resp.StatusCode)
	ctx.Header("Accept-Ranges", resp.Header.Get("Accept-Ranges"))
	ctx.Header("Cache-Control", resp.Header.Get("Cache-Control"))
	ctx.Header("Content-Length", resp.Header.Get("Content-Length"))
	ctx.Header("Content-Range", resp.Header.Get("Content-Range"))
	ctx.Header("Content-Type", resp.Header.Get("Content-Type"))
	_, err = copyBuffer(ctx.Writer, resp.Body)
	if err != nil && err != io.EOF {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewApiErrorStringResp(
				fmt.Sprintf("copy response body error: %v", err),
			),
		)
		return fmt.Errorf("copy response body error: %w", err)
	}
	return nil
}

func AutoProxyURL(ctx *gin.Context, u, t string, headers map[string]string, cache bool, token, roomId, movieId string) error {
	if strings.HasPrefix(t, "m3u") || utils.IsM3u8Url(u) {
		return ProxyM3u8(ctx, u, headers, cache, true, token, roomId, movieId)
	}
	return ProxyURL(ctx, u, headers, cache)
}
