package proxy

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/go-uhc"
)

func ProxyURL(ctx *gin.Context, u string, headers map[string]string) error {
	if !settings.AllowProxyToLocal.Get() {
		if l, err := utils.ParseURLIsLocalIP(u); err != nil {
			return fmt.Errorf("check url is local ip error: %w", err)
		} else if l {
			return errors.New("not allow proxy to local")
		}
	}
	ctx2, cf := context.WithCancel(ctx)
	defer cf()
	req, err := http.NewRequestWithContext(ctx2, http.MethodGet, u, nil)
	if err != nil {
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
		return fmt.Errorf("copy response body error: %w", err)
	}
	return nil
}

func AuthProxyURL(ctx *gin.Context, u, t string, headers map[string]string, token, roomId, movieId string) error {
	if strings.HasPrefix(t, "m3u") || utils.IsM3u8Url(u) {
		return ProxyM3u8(ctx, u, headers, true, token, roomId, movieId)
	}
	return ProxyURL(ctx, u, headers)
}
