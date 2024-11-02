package proxy

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
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
	defaultCache  *MemoryCache
	fileCacheOnce sync.Once
	fileCache     Cache
)

// MB GB KB
func parseProxyCacheSize(sizeStr string) (int64, error) {
	if sizeStr == "" {
		return 0, nil
	}
	sizeStr = strings.ToLower(sizeStr)
	sizeStr = strings.TrimSpace(sizeStr)

	var multiplier int64 = 1024 * 1024 // Default MB

	if strings.HasSuffix(sizeStr, "gb") {
		multiplier = 1024 * 1024 * 1024
		sizeStr = strings.TrimSuffix(sizeStr, "gb")
	} else if strings.HasSuffix(sizeStr, "mb") {
		multiplier = 1024 * 1024
		sizeStr = strings.TrimSuffix(sizeStr, "mb")
	} else if strings.HasSuffix(sizeStr, "kb") {
		multiplier = 1024
		sizeStr = strings.TrimSuffix(sizeStr, "kb")
	}

	size, err := strconv.ParseInt(strings.TrimSpace(sizeStr), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid size format: %w", err)
	}

	return size * multiplier, nil
}

func getCache() Cache {
	fileCacheOnce.Do(func() {
		size, err := parseProxyCacheSize(conf.Conf.Server.ProxyCacheSize)
		if err != nil {
			log.Fatalf("parse proxy cache size error: %v", err)
		}
		if size == 0 {
			size = 1024 * 1024 * 1024
		}
		if conf.Conf.Server.ProxyCachePath == "" {
			log.Infof("proxy cache path is empty, use memory cache, size: %d", size)
			defaultCache = NewMemoryCache(0, WithMaxSizeBytes(size))
			return
		}
		log.Infof("proxy cache path: %s, size: %d", conf.Conf.Server.ProxyCachePath, size)
		fileCache = NewFileCache(conf.Conf.Server.ProxyCachePath, WithFileCacheMaxSizeBytes(size))
	})
	if fileCache != nil {
		return fileCache
	}
	return defaultCache
}

type ProxyURLOptions struct {
	CacheKey string
	Cache    bool
}

type ProxyURLOption func(o *ProxyURLOptions)

func WithProxyURLCache(cache bool) ProxyURLOption {
	return func(o *ProxyURLOptions) {
		o.Cache = cache
	}
}

func WithProxyURLCacheKey(key string) ProxyURLOption {
	return func(o *ProxyURLOptions) {
		o.CacheKey = key
	}
}

func NewProxyURLOptions(opts ...ProxyURLOption) *ProxyURLOptions {
	o := &ProxyURLOptions{}
	for _, opt := range opts {
		opt(o)
	}
	return o
}

func ProxyURL(ctx *gin.Context, u string, headers map[string]string, opts ...ProxyURLOption) error {
	o := NewProxyURLOptions(opts...)
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

	if o.Cache && settings.ProxyCacheEnable.Get() {
		c, cancel := context.WithCancel(ctx)
		defer cancel()
		rsc := NewHttpReadSeekCloser(u,
			WithContext(c),
			WithHeadersMap(headers),
		)
		defer rsc.Close()
		if o.CacheKey == "" {
			o.CacheKey = u
		}
		return NewSliceCacheProxy(o.CacheKey, 1024*512, rsc, getCache()).
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
			for k, v := range headers {
				req.Header.Set(k, v)
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

func AutoProxyURL(ctx *gin.Context, u, t string, headers map[string]string, token, roomId, movieId string, opts ...ProxyURLOption) error {
	if strings.HasPrefix(t, "m3u") || utils.IsM3u8Url(u) {
		return ProxyM3u8(ctx, u, headers, true, token, roomId, movieId, opts...)
	}
	return ProxyURL(ctx, u, headers, opts...)
}
