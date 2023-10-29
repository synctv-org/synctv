package op

import (
	"errors"
	"net/url"
	"sync"
	"time"

	"github.com/go-resty/resty/v2"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/av"
	"github.com/zijiren233/livelib/container/flv"
	rtmpProto "github.com/zijiren233/livelib/protocol/rtmp"
	"github.com/zijiren233/livelib/protocol/rtmp/core"
	rtmps "github.com/zijiren233/livelib/server"
)

type movie struct {
	*model.Movie
	lock    sync.RWMutex
	channel *rtmps.Channel
}

func (m *movie) Channel() (*rtmps.Channel, error) {
	m.lock.Lock()
	defer m.lock.Unlock()
	return m.channel, m.init()
}

func (m *movie) init() (err error) {
	switch {
	case m.Base.RtmpSource && m.Base.Proxy:
		return errors.New("rtmp source and proxy can't be true at the same time")
	case m.Base.Live && m.Base.RtmpSource:
		if !conf.Conf.Rtmp.Enable {
			return errors.New("rtmp is not enabled")
		}
		if m.channel == nil {
			m.channel = rtmps.NewChannel()
			m.channel.InitHlsPlayer()
		}
	case m.Base.Live && m.Base.Proxy:
		if !conf.Conf.Proxy.LiveProxy {
			return errors.New("live proxy is not enabled")
		}
		u, err := url.Parse(m.Base.Url)
		if err != nil {
			return err
		}
		if utils.IsLocalIP(u.Host) {
			return errors.New("local ip is not allowed")
		}
		switch u.Scheme {
		case "rtmp":
			if m.channel == nil {
				m.channel = rtmps.NewChannel()
				m.channel.InitHlsPlayer()
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
			if m.Base.Type != "flv" {
				return errors.New("only flv is supported")
			}
			if m.channel == nil {
				m.channel = rtmps.NewChannel()
				m.channel.InitHlsPlayer()
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
	case !m.Base.Live && m.Base.RtmpSource:
		return errors.New("rtmp source can't be true when movie is not live")
	case !m.Base.Live && m.Base.Proxy:
		if !conf.Conf.Proxy.MovieProxy {
			return errors.New("movie proxy is not enabled")
		}
		if m.Base.VendorInfo.Vendor != "" {
			return errors.New("vendor movie info is not supported in movie proxy mode")
		}
		u, err := url.Parse(m.Base.Url)
		if err != nil {
			return err
		}
		if utils.IsLocalIP(u.Host) {
			return errors.New("local ip is not allowed")
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			return errors.New("unsupported scheme")
		}
	case !m.Base.Live && !m.Base.Proxy, m.Base.Live && !m.Base.Proxy && !m.Base.RtmpSource:
		if m.Base.VendorInfo.Vendor == "" {
			u, err := url.Parse(m.Base.Url)
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

func (m *movie) Terminate() {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.terminate()
}

func (m *movie) terminate() {
	if m.channel != nil {
		m.channel.Close()
		m.channel = nil
	}
}

func (m *movie) Update(movie model.BaseMovie) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.terminate()
	m.Movie.Base = movie
	return m.init()
}
