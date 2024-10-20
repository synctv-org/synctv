package proxy

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/settings"
	cache "github.com/synctv-org/synctv/proxy"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/go-uhc"
)

var memoryCache = cache.NewMemoryCache()

func ProxyURL(ctx *gin.Context, u string, headers map[string]string) error {
	if utils.GetUrlExtension(u) == "m3u8" {
		ctx.Redirect(http.StatusFound, u)
		return nil
	}
	if !settings.AllowProxyToLocal.Get() {
		if l, err := utils.ParseURLIsLocalIP(u); err != nil {
			return fmt.Errorf("check url is local ip error: %w", err)
		} else if l {
			return errors.New("not allow proxy to local")
		}
	}
	cli := http.Client{
		Transport: uhc.DefaultTransport,
		CheckRedirect: func(req *http.Request, via []*http.Request) error {
			req.Header.Del("Referer")
			for k, v := range headers {
				req.Header.Set(k, v)
			}
			req.Header.Set("Range", ctx.GetHeader("Range"))
			req.Header.Set("Accept-Encoding", ctx.GetHeader("Accept-Encoding"))
			if req.Header.Get("User-Agent") == "" {
				req.Header.Set("User-Agent", utils.UA)
			}
			return nil
		},
	}
	s, err := cache.NewHttpReadSeekCloser(u, cache.WithClient(&cli), cache.WithHeadMethod(http.MethodGet))
	if err != nil {
		return fmt.Errorf("create http read seek closer error: %w", err)
	}
	defer s.Close()
	http.ServeContent(ctx.Writer, ctx.Request, "", time.Now(), cache.NewCachedReadSeeker(u, 16*1024, s, memoryCache))
	return nil
}

func copyBuffer(dst io.Writer, src io.Reader) (written int64, err error) {
	buf := getBuffer()
	defer putBuffer(buf)
	for {
		nr, er := src.Read(*buf)
		if nr > 0 {
			nw, ew := dst.Write((*buf)[0:nr])
			if nw < 0 || nr < nw {
				nw = 0
				if ew == nil {
					ew = errors.New("invalid write result")
				}
			}
			written += int64(nw)
			if ew != nil {
				err = ew
				break
			}
			if nr != nw {
				err = io.ErrShortWrite
				break
			}
		}
		if er != nil {
			if er != io.EOF {
				err = er
			}
			break
		}
	}
	return written, err
}
