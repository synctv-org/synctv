package op

import (
	"errors"
	"fmt"
	"net/url"
	"sync/atomic"
	"time"

	"github.com/go-resty/resty/v2"
	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/av"
	"github.com/zijiren233/livelib/container/flv"
	"github.com/zijiren233/livelib/protocol/hls"
	rtmpProto "github.com/zijiren233/livelib/protocol/rtmp"
	"github.com/zijiren233/livelib/protocol/rtmp/core"
	rtmps "github.com/zijiren233/livelib/server"
)

type Movie struct {
	Movie         model.Movie
	channel       atomic.Pointer[rtmps.Channel]
	bilibiliCache atomic.Pointer[cache.BilibiliMovieCache]
	alistCache    atomic.Pointer[cache.AlistMovieCache]
}

func (m *Movie) BilibiliCache() *cache.BilibiliMovieCache {
	c := m.bilibiliCache.Load()
	if c == nil {
		c = cache.NewBilibiliMovieCache(&m.Movie)
		if !m.bilibiliCache.CompareAndSwap(nil, c) {
			return m.BilibiliCache()
		}
	}
	return c
}

func (m *Movie) AlistCache() *cache.AlistMovieCache {
	c := m.alistCache.Load()
	if c == nil {
		c = cache.NewAlistMovieCache(&m.Movie)
		if !m.alistCache.CompareAndSwap(nil, c) {
			return m.AlistCache()
		}
	}
	return c
}

func (m *Movie) Channel() (*rtmps.Channel, error) {
	return m.channel.Load(), m.initChannel()
}

func genTsName() string {
	return utils.SortUUID()
}

func (m *Movie) compareAndSwapInitChannel() (*rtmps.Channel, bool) {
	if m.channel.Load() == nil {
		c := rtmps.NewChannel()
		if !m.channel.CompareAndSwap(nil, c) {
			return nil, false
		}
		return c, true
	}
	return nil, false
}

func (m *Movie) initChannel() error {
	switch {
	case m.Movie.Base.Live && m.Movie.Base.RtmpSource:
		if c, ok := m.compareAndSwapInitChannel(); ok {
			return c.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
		}
	case m.Movie.Base.Live && m.Movie.Base.Proxy:
		u, err := url.Parse(m.Movie.Base.Url)
		if err != nil {
			return err
		}
		switch u.Scheme {
		case "rtmp":
			c, ok := m.compareAndSwapInitChannel()
			if !ok {
				return nil
			}
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
					if err = cli.Start(m.Movie.Base.Url, av.PLAY); err != nil {
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
			c, ok := m.compareAndSwapInitChannel()
			if !ok {
				return nil
			}
			c.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
			go func() {
				for {
					if c.Closed() {
						return
					}
					r := resty.New().R()
					for k, v := range m.Movie.Base.Headers {
						r.SetHeader(k, v)
					}
					// r.SetHeader("User-Agent", UserAgent)
					resp, err := r.Get(m.Movie.Base.Url)
					if err != nil {
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(flv.NewReader(resp.RawBody())); err != nil {
						time.Sleep(time.Second)
					}
					resp.RawBody().Close()
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
	m := movie.Movie.Base
	if m.VendorInfo.Vendor != "" {
		err := movie.validateVendorMovie()
		if err != nil {
			return err
		}
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
	switch movie.Movie.Base.VendorInfo.Vendor {
	case model.VendorBilibili:
		return movie.Movie.Base.VendorInfo.Bilibili.Validate()

	case model.VendorAlist:
		// return movie.Movie.Base.VendorInfo.Alist.Validate()

	default:
		return fmt.Errorf("vendor not support")
	}

	return nil
}

func (m *Movie) Terminate() error {
	c := m.channel.Swap(nil)
	if c != nil {
		err := c.Close()
		if err != nil {
			return err
		}
	}
	bmc := m.bilibiliCache.Swap(nil)
	if bmc != nil {
		bmc.NoSharedMovie.Clear()
	}
	return nil
}

func (m *Movie) Update(movie *model.BaseMovie) error {
	m.Movie.Base = *movie
	return m.Terminate()
}
