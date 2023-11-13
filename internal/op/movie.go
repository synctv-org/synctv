package op

import (
	"errors"
	"fmt"
	"net/url"
	"sync"
	"time"

	"github.com/go-resty/resty/v2"
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
	*model.Movie
	lock    sync.RWMutex
	channel *rtmps.Channel
}

func (m *Movie) Channel() (*rtmps.Channel, error) {
	m.lock.Lock()
	defer m.lock.Unlock()
	return m.channel, m.init()
}

func genTsName() string {
	return utils.SortUUID()
}

func (m *Movie) init() (err error) {
	if err = m.Validate(); err != nil {
		return
	}
	switch {
	case m.Base.Live && m.Base.RtmpSource:
		if m.channel == nil {
			m.channel = rtmps.NewChannel()
			m.channel.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
		}
	case m.Base.Live && m.Base.Proxy:
		u, err := url.Parse(m.Base.Url)
		if err != nil {
			return err
		}
		switch u.Scheme {
		case "rtmp":
			if m.channel == nil {
				m.channel = rtmps.NewChannel()
				m.channel.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
				go func() {
					for {
						if m.channel.Closed() {
							return
						}
						cli := core.NewConnClient()
						if err = cli.Start(m.Base.Url, av.PLAY); err != nil {
							cli.Close()
							time.Sleep(time.Second)
							continue
						}
						if err := m.channel.PushStart(rtmpProto.NewReader(cli)); err != nil {
							cli.Close()
							time.Sleep(time.Second)
						}
					}
				}()
			}
		case "http", "https":
			if m.channel == nil {
				m.channel = rtmps.NewChannel()
				m.channel.InitHlsPlayer(hls.WithGenTsNameFunc(genTsName))
				go func() {
					for {
						if m.channel.Closed() {
							return
						}
						r := resty.New().R()
						for k, v := range m.Base.Headers {
							r.SetHeader(k, v)
						}
						// r.SetHeader("User-Agent", UserAgent)
						resp, err := r.Get(m.Base.Url)
						if err != nil {
							time.Sleep(time.Second)
							continue
						}
						if err := m.channel.PushStart(flv.NewReader(resp.RawBody())); err != nil {
							time.Sleep(time.Second)
						}
						resp.RawBody().Close()
					}
				}()
			}
		default:
			return errors.New("unsupported scheme")
		}
	}
	return nil
}

func (movie *Movie) Validate() error {
	m := movie.Base
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
	m := movie.Base
	switch m.VendorInfo.Vendor {
	case model.StreamingVendorBilibili:
		err := m.VendorInfo.Bilibili.Validate()
		if err != nil {
			return err
		}
		if m.Headers == nil {
			m.Headers = map[string]string{
				"Referer":    "https://www.bilibili.com",
				"User-Agent": utils.UA,
			}
		} else {
			m.Headers["Referer"] = "https://www.bilibili.com"
			m.Headers["User-Agent"] = utils.UA
		}
	default:
		return fmt.Errorf("vendor not support")
	}

	return nil
}

func (m *Movie) Terminate() {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.terminate()
}

func (m *Movie) terminate() {
	if m.channel != nil {
		m.channel.Close()
		m.channel = nil
	}
	m.Cache.Clear()
}

func (m *Movie) Update(movie model.BaseMovie) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.terminate()
	m.Movie.Base = movie
	return m.init()
}
