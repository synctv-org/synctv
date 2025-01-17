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

	log "github.com/sirupsen/logrus"
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

func (m *Movie) ExpireID(ctx context.Context) (uint64, error) {
	switch {
	case m.Movie.MovieBase.VendorInfo.Vendor == model.VendorAlist:
		amcd, _ := m.AlistCache().Raw()
		if amcd != nil && amcd.Ali != nil {
			return uint64(amcd.Ali.Last()), nil
		}
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.VendorInfo.Vendor == model.VendorBilibili:
		liveCache := m.BilibiliCache().Live
		_, err := liveCache.Get(ctx)
		if err != nil {
			return 0, err
		}
		return uint64(liveCache.Last()), nil
	}
	return uint64(crc32.ChecksumIEEE([]byte(m.Movie.ID))), nil
}

func (m *Movie) CheckExpired(ctx context.Context, expireID uint64) (bool, error) {
	switch {
	case m.Movie.MovieBase.VendorInfo.Vendor == model.VendorAlist:
		amcd, _ := m.AlistCache().Raw()
		if amcd != nil && amcd.Ali != nil {
			return time.Now().UnixNano()-int64(amcd.Ali.Last()) > amcd.Ali.MaxAge(), nil
		}
	case m.Movie.MovieBase.Live && m.Movie.MovieBase.VendorInfo.Vendor == model.VendorBilibili:
		return time.Now().UnixNano()-int64(expireID) > m.BilibiliCache().Live.MaxAge(), nil
	}
	id, err := m.ExpireID(ctx)
	if err != nil {
		return false, err
	}
	return expireID != id, nil
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
	c, err := m.initChannel()
	if err != nil {
		return nil, err
	}
	return c, nil
}

func genTSName() string {
	return utils.SortUUID()
}

func (m *Movie) compareAndSwapInitChannel() (*rtmps.Channel, bool) {
	c := m.channel.Load()
	if c == nil {
		c = rtmps.NewChannel()
		if !m.channel.CompareAndSwap(nil, c) {
			return m.compareAndSwapInitChannel()
		}
		return c, true
	}
	return c, false
}

func (m *Movie) initChannel() (*rtmps.Channel, error) {
	if !m.Movie.MovieBase.Live || (!m.Movie.MovieBase.RtmpSource && !m.Movie.MovieBase.Proxy) {
		return nil, errors.New("this movie not support channel")
	}

	if m.Movie.MovieBase.RtmpSource {
		return m.initRtmpSourceChannel()
	}

	// Handle proxy case
	u, err := url.Parse(m.Movie.MovieBase.URL)
	if err != nil {
		return nil, err
	}

	switch u.Scheme {
	case "rtmp":
		return m.initRtmpProxyChannel()
	case "http", "https":
		return m.initHTTPProxyChannel()
	default:
		return nil, fmt.Errorf("unsupported scheme: %s", u.Scheme)
	}
}

func (m *Movie) initRtmpSourceChannel() (*rtmps.Channel, error) {
	c, init := m.compareAndSwapInitChannel()
	if !init {
		return c, nil
	}
	err := c.InitHlsPlayer(hls.WithGenTsNameFunc(genTSName))
	if err != nil {
		return nil, fmt.Errorf("init rtmp hls player error: %w", err)
	}
	return c, nil
}

func (m *Movie) initRtmpProxyChannel() (*rtmps.Channel, error) {
	c, init := m.compareAndSwapInitChannel()
	if !init {
		return c, nil
	}
	err := c.InitHlsPlayer(hls.WithGenTsNameFunc(genTSName))
	if err != nil {
		return nil, fmt.Errorf("init rtmp hls player error: %w", err)
	}

	go m.handleRtmpProxy(c)
	return c, nil
}

func (m *Movie) handleRtmpProxy(c *rtmps.Channel) {
	for {
		if c.Closed() {
			return
		}
		cli := core.NewConnClient()
		if err := cli.Start(m.Movie.MovieBase.URL, av.PLAY); err != nil {
			log.Errorf("push live error: %v", err)
			cli.Close()
			time.Sleep(time.Second)
			continue
		}
		if err := c.PushStart(rtmpProto.NewReader(cli)); err != nil {
			log.Errorf("push live error: %v", err)
			cli.Close()
			time.Sleep(time.Second)
		}
	}
}

func (m *Movie) initHTTPProxyChannel() (*rtmps.Channel, error) {
	if utils.IsM3u8Url(m.Movie.MovieBase.URL) {
		return nil, errors.New("m3u8 url not support")
	}

	c, init := m.compareAndSwapInitChannel()
	if !init {
		return c, nil
	}
	err := c.InitHlsPlayer(hls.WithGenTsNameFunc(genTSName))
	if err != nil {
		return nil, fmt.Errorf("init http hls player error: %w", err)
	}

	go m.handleHTTPProxy(c)
	return c, nil
}

func (m *Movie) handleHTTPProxy(c *rtmps.Channel) {
	for {
		if c.Closed() {
			return
		}
		req, err := http.NewRequestWithContext(context.Background(), http.MethodGet, m.Movie.MovieBase.URL, nil)
		if err != nil {
			log.Errorf("get live error: %v", err)
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
			log.Errorf("get live error: %v", err)
			resp.Body.Close()
			time.Sleep(time.Second)
			continue
		}
		if err := c.PushStart(flv.NewReader(resp.Body)); err != nil {
			log.Errorf("push live error: %v", err)
			resp.Body.Close()
			time.Sleep(time.Second)
		}
	}
}

func (m *Movie) Validate() error {
	// First check vendor info
	if m.VendorInfo.Vendor != "" {
		return m.validateVendorMovie()
	}

	// Check folder
	if m.IsFolder {
		return nil
	}

	// Validate RTMP source settings
	if err := m.validateRTMPSource(); err != nil {
		return err
	}

	// Validate URL and proxy settings
	return m.validateURLAndProxy()
}

func (m *Movie) validateRTMPSource() error {
	switch {
	case m.RtmpSource && m.Proxy:
		return errors.New("rtmp source and proxy can't be true at the same time")
	case m.Live && m.RtmpSource && !conf.Conf.Server.RTMP.Enable:
		return errors.New("rtmp is not enabled")
	case !m.Live && m.RtmpSource:
		return errors.New("rtmp source can't be true when movie is not live")
	}
	return nil
}

func (m *Movie) validateURLAndProxy() error {
	u, err := url.Parse(m.URL)
	if err != nil {
		return err
	}

	switch {
	case m.Live && m.RtmpSource:
		return nil
	case m.Live && m.Proxy:
		return m.validateLiveProxy(u)
	case !m.Live && m.Proxy:
		return m.validateMovieProxy(u)
	case !m.Live && !m.Proxy, m.Live && !m.Proxy && !m.RtmpSource:
		return m.validateDirectURL(u)
	default:
		return errors.New("validate movie error: unknown error")
	}
}

func (m *Movie) validateLiveProxy(u *url.URL) error {
	if !settings.LiveProxy.Get() {
		return errors.New("live proxy is not enabled")
	}
	if !settings.AllowProxyToLocal.Get() && utils.IsLocalIP(u.Host) {
		return errors.New("local ip is not allowed")
	}
	switch u.Scheme {
	case "rtmp", "http", "https":
		return nil
	default:
		return fmt.Errorf("unsupported scheme: %s", u.Scheme)
	}
}

func (m *Movie) validateMovieProxy(u *url.URL) error {
	if !settings.MovieProxy.Get() {
		return errors.New("movie proxy is not enabled")
	}
	if !settings.AllowProxyToLocal.Get() && utils.IsLocalIP(u.Host) {
		return errors.New("local ip is not allowed")
	}
	if u.Scheme != "http" && u.Scheme != "https" {
		return fmt.Errorf("unsupported scheme: %s", u.Scheme)
	}
	return nil
}

func (m *Movie) validateDirectURL(u *url.URL) error {
	if u.Scheme != "http" && u.Scheme != "https" && u.Scheme != "magnet" {
		return fmt.Errorf("unsupported scheme: %s", u.Scheme)
	}
	return nil
}

func (m *Movie) validateVendorMovie() error {
	switch m.Movie.MovieBase.VendorInfo.Vendor {
	case model.VendorBilibili:
		if m.IsFolder {
			return errors.New("bilibili folder not support")
		}
		return m.Movie.MovieBase.VendorInfo.Bilibili.Validate()

	case model.VendorAlist:
		return m.Movie.MovieBase.VendorInfo.Alist.Validate()

	case model.VendorEmby:
		return m.Movie.MovieBase.VendorInfo.Emby.Validate()

	default:
		return errors.New("vendor not implement validate")
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
