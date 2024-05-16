package op

import (
	"context"
	"errors"
	"fmt"
	"hash/crc32"
	"net/http"
	"net/url"
	"sync/atomic"
	"time"

	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/go-uhc"
	"github.com/zijiren233/livelib/av"
	"github.com/zijiren233/livelib/container/flv"
	"github.com/zijiren233/livelib/protocol/hls"
	rtmpProto "github.com/zijiren233/livelib/protocol/rtmp"
	"github.com/zijiren233/livelib/protocol/rtmp/core"
	rtmps "github.com/zijiren233/livelib/server"
)

type Movie struct {
	*model.Movie
	channel       atomic.Pointer[rtmps.Channel]
	alistCache    atomic.Pointer[cache.AlistMovieCache]
	bilibiliCache atomic.Pointer[cache.BilibiliMovieCache]
	embyCache     atomic.Pointer[cache.EmbyMovieCache]
	subPath       string
}

func (m *Movie) SubPath() string {
	return m.subPath
}

func (m *Movie) ExpireId() uint64 {
	if m.IsFolder {
		return 0
	}
	switch {
	case m.Movie.MovieBase.VendorInfo.Vendor == model.VendorAlist:
		amcd, _ := m.AlistCache().Raw()
		if amcd != nil && amcd.Ali != nil {
			return uint64(m.AlistCache().Last())
		}
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.VendorInfo.Vendor == model.VendorBilibili:
		return uint64(m.BilibiliCache().Live.Last())
	}
	return uint64(crc32.ChecksumIEEE([]byte(m.Movie.ID)))
}

func (m *Movie) CheckExpired(expireId uint64) bool {
	if m.IsFolder {
		return false
	}
	switch {
	case m.Movie.MovieBase.VendorInfo.Vendor == model.VendorAlist:
		amcd, _ := m.AlistCache().Raw()
		if amcd != nil && amcd.Ali != nil {
			return time.Now().UnixNano()-int64(expireId) > m.AlistCache().MaxAge()
		}
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.VendorInfo.Vendor == model.VendorBilibili:
		return time.Now().UnixNano()-int64(expireId) > m.BilibiliCache().Live.MaxAge()
	}
	return expireId != m.ExpireId()
}

func (m *Movie) ClearCache() error {
	m.alistCache.Store(nil)

	bmc := m.bilibiliCache.Swap(nil)
	if bmc != nil {
		bmc.NoSharedMovie.Clear()
	}

	emc := m.embyCache.Swap(nil)
	if emc != nil {
		u, err := LoadOrInitUserByID(m.CreatorID)
		if err != nil {
			return err
		}
		err = emc.Clear(context.Background(), u.Value().EmbyCache())
		if err != nil {
			return err
		}
	}

	return nil
}

func (m *Movie) AlistCache() *cache.AlistMovieCache {
	c := m.alistCache.Load()
	if c == nil {
		c = cache.NewAlistMovieCache(m.Movie, m.subPath)
		if !m.alistCache.CompareAndSwap(nil, c) {
			return m.AlistCache()
		}
	}
	return c
}

func (m *Movie) BilibiliCache() *cache.BilibiliMovieCache {
	c := m.bilibiliCache.Load()
	if c == nil {
		c = cache.NewBilibiliMovieCache(m.Movie)
		if !m.bilibiliCache.CompareAndSwap(nil, c) {
			return m.BilibiliCache()
		}
	}
	return c
}

func (m *Movie) EmbyCache() *cache.EmbyMovieCache {
	c := m.embyCache.Load()
	if c == nil {
		c = cache.NewEmbyMovieCache(m.Movie, m.subPath)
		if !m.embyCache.CompareAndSwap(nil, c) {
			return m.EmbyCache()
		}
	}
	return c
}

func (m *Movie) Channel() (*rtmps.Channel, error) {
	if m.IsFolder {
		return nil, errors.New("this is a folder")
	}
	err := m.initChannel()
	if err != nil {
		return nil, err
	}
	return m.channel.Load(), nil
}

func genTsName() string {
	return utils.SortUUID()
}

func (m *Movie) compareAndSwapInitChannel() *rtmps.Channel {
	c := m.channel.Load()
	if c == nil {
		c = rtmps.NewChannel()
		if !m.channel.CompareAndSwap(nil, c) {
			return m.compareAndSwapInitChannel()
		}
		c.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
	}
	return c
}

func (m *Movie) initChannel() error {
	switch {
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.RtmpSource:
		m.compareAndSwapInitChannel()
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.Proxy:
		u, err := url.Parse(m.Movie.MovieBase.Url)
		if err != nil {
			return err
		}
		switch u.Scheme {
		case "rtmp":
			c := m.compareAndSwapInitChannel()
			err = c.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
			if err != nil {
				return err
			}
			go func() {
				for {
					if c.Closed() {
						return
					}
					cli := core.NewConnClient()
					if err = cli.Start(m.Movie.MovieBase.Url, av.PLAY); err != nil {
						cli.Close()
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(rtmpProto.NewReader(cli)); err != nil {
						cli.Close()
						time.Sleep(time.Second)
					}
				}
			}()
		case "http", "https":
			c := m.compareAndSwapInitChannel()
			err := c.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
			if err != nil {
				return err
			}
			go func() {
				for {
					if c.Closed() {
						return
					}
					req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, m.Movie.MovieBase.Url, nil)
					if err != nil {
						time.Sleep(time.Second)
						continue
					}
					for k, v := range m.Movie.MovieBase.Headers {
						req.Header.Set(k, v)
					}
					if req.Header.Get("User-Agent") == "" {
						req.Header.Set("User-Agent", utils.UA)
					}
					resp, err := uhc.Do(req)
					if err != nil {
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(flv.NewReader(resp.Body)); err != nil {
						time.Sleep(time.Second)
					}
					resp.Body.Close()
				}
			}()
		default:
			return errors.New("unsupported scheme")
		}
	default:
		return errors.New("this movie not support channel")
	}
	return nil
}

func (movie *Movie) Validate() error {
	m := movie.Movie.MovieBase
	if m.VendorInfo.Vendor != "" {
		err := movie.validateVendorMovie()
		if err != nil {
			return err
		}
	}
	if movie.IsFolder {
		return nil
	}
	switch {
	case m.RtmpSource && m.Proxy:
		return errors.New("rtmp source and proxy can't be true at the same time")
	case m.Live && m.RtmpSource:
		if !conf.Conf.Server.Rtmp.Enable {
			return errors.New("rtmp is not enabled")
		}
	case m.Live && m.Proxy:
		if !settings.LiveProxy.Get() {
			return errors.New("live proxy is not enabled")
		}
		u, err := url.Parse(m.Url)
		if err != nil {
			return err
		}
		if !settings.AllowProxyToLocal.Get() && utils.IsLocalIP(u.Host) {
			return errors.New("local ip is not allowed")
		}
		switch u.Scheme {
		case "rtmp":
		case "http", "https":
		default:
			return errors.New("unsupported scheme")
		}
	case !m.Live && m.RtmpSource:
		return errors.New("rtmp source can't be true when movie is not live")
	case !m.Live && m.Proxy:
		if !settings.MovieProxy.Get() {
			return errors.New("movie proxy is not enabled")
		}
		if m.VendorInfo.Vendor != "" {
			return nil
		}
		u, err := url.Parse(m.Url)
		if err != nil {
			return err
		}
		if !settings.AllowProxyToLocal.Get() && utils.IsLocalIP(u.Host) {
			return errors.New("local ip is not allowed")
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			return errors.New("unsupported scheme")
		}
	case !m.Live && !m.Proxy, m.Live && !m.Proxy && !m.RtmpSource:
		if m.VendorInfo.Vendor == "" {
			u, err := url.Parse(m.Url)
			if err != nil {
				return err
			}
			if u.Scheme != "http" && u.Scheme != "https" {
				return errors.New("unsupported scheme")
			}
		}
	default:
		return errors.New("unknown error")
	}
	return nil
}

func (movie *Movie) validateVendorMovie() error {
	switch movie.Movie.MovieBase.VendorInfo.Vendor {
	case model.VendorBilibili:
		if movie.IsFolder {
			return errors.New("bilibili folder not support")
		}
		return movie.Movie.MovieBase.VendorInfo.Bilibili.Validate()

	case model.VendorAlist:
		return movie.Movie.MovieBase.VendorInfo.Alist.Validate()

	case model.VendorEmby:
		return movie.Movie.MovieBase.VendorInfo.Emby.Validate()

	default:
		return fmt.Errorf("vendor not implement validate")
	}
}

func (m *Movie) Terminate() error {
	if m.IsFolder {
		return nil
	}
	c := m.channel.Swap(nil)
	if c != nil {
		err := c.Close()
		if err != nil {
			return err
		}
	}
	return nil
}

func (m *Movie) Close() error {
	err := m.Terminate()
	if err != nil {
		return err
	}
	err = m.ClearCache()
	if err != nil {
		return err
	}
	return nil
}
