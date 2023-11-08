package model

import (
	"errors"
	"fmt"
	"net/url"
	"sync/atomic"
	"time"

	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/utils"
	refreshcache "github.com/synctv-org/synctv/utils/refreshCache"
	"github.com/zijiren233/gencontainer/rwmap"
	"gorm.io/gorm"
)

type Movie struct {
	ID        string    `gorm:"primaryKey;type:varchar(32)" json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	Position  uint      `gorm:"not null" json:"-"`
	RoomID    string    `gorm:"not null;index" json:"-"`
	CreatorID string    `gorm:"not null;index" json:"creatorId"`
	Base      BaseMovie `gorm:"embedded;embeddedPrefix:base_" json:"base"`
}

func (m *Movie) BeforeCreate(tx *gorm.DB) error {
	if m.ID == "" {
		m.ID = utils.SortUUID()
	}
	return nil
}

func (m *Movie) Validate() error {
	return m.Base.Validate()
}

type BaseMovie struct {
	Url        string            `json:"url"`
	Name       string            `gorm:"not null" json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `json:"type"`
	Headers    map[string]string `gorm:"serializer:fastjson" json:"headers"`
	VendorInfo VendorInfo        `gorm:"embedded;embeddedPrefix:vendor_info_" json:"vendorInfo,omitempty"`
}

func (m *BaseMovie) Validate() error {
	if m.VendorInfo.Vendor != "" {
		err := m.validateVendorMovie()
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
		if !conf.Conf.Proxy.LiveProxy {
			return errors.New("live proxy is not enabled")
		}
		u, err := url.Parse(m.Url)
		if err != nil {
			return err
		}
		if !conf.Conf.Proxy.AllowProxyToLocal && utils.IsLocalIP(u.Host) {
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
		if !conf.Conf.Proxy.MovieProxy {
			return errors.New("movie proxy is not enabled")
		}
		if m.VendorInfo.Vendor != "" {
			return nil
		}
		u, err := url.Parse(m.Url)
		if err != nil {
			return err
		}
		if !conf.Conf.Proxy.AllowProxyToLocal && utils.IsLocalIP(u.Host) {
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

func (m *BaseMovie) validateVendorMovie() error {
	switch m.VendorInfo.Vendor {
	case StreamingVendorBilibili:
		info := m.VendorInfo.Bilibili
		if info.Bvid == "" && info.Epid == 0 {
			return fmt.Errorf("bvid and epid are empty")
		}

		if info.Bvid != "" && info.Epid != 0 {
			return fmt.Errorf("bvid and epid can't be set at the same time")
		}

		if info.Bvid != "" && info.Cid == 0 {
			return fmt.Errorf("cid is empty")
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

type VendorInfo struct {
	Vendor   StreamingVendor     `json:"vendor"`
	Shared   bool                `gorm:"not null;default:false" json:"shared"`
	Bilibili *BilibiliVendorInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
}

type BilibiliVendorInfo struct {
	Bvid    string              `json:"bvid,omitempty"`
	Cid     uint                `json:"cid,omitempty"`
	Epid    uint                `json:"epid,omitempty"`
	Quality uint                `json:"quality,omitempty"`
	Cache   BilibiliVendorCache `gorm:"-:all" json:"-"`
}

type BilibiliVendorCache struct {
	URL rwmap.RWMap[string, *refreshcache.RefreshCache[string]]
	MPD atomic.Pointer[refreshcache.RefreshCache[*MPDCache]]
}

type MPDCache struct {
	MPDFile string
	URLs    []string
}

func (b *BilibiliVendorInfo) InitOrLoadURLCache(id string, initCache func(*BilibiliVendorInfo) (*refreshcache.RefreshCache[string], error)) (*refreshcache.RefreshCache[string], error) {
	if c, loaded := b.Cache.URL.Load(id); loaded {
		return c, nil
	}
	c, err := initCache(b)
	if err != nil {
		return nil, err
	}

	c, _ = b.Cache.URL.LoadOrStore(id, c)

	return c, nil
}

func (b *BilibiliVendorInfo) InitOrLoadMPDCache(initCache func(*BilibiliVendorInfo) (*refreshcache.RefreshCache[*MPDCache], error)) (*refreshcache.RefreshCache[*MPDCache], error) {
	if c := b.Cache.MPD.Load(); c != nil {
		return c, nil
	}
	c, err := initCache(b)
	if err != nil {
		return nil, err
	}
	if b.Cache.MPD.CompareAndSwap(nil, c) {
		return c, nil
	} else {
		return b.InitOrLoadMPDCache(initCache)
	}
}
