package model

import (
	"fmt"
	"sync/atomic"
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/refreshcache"
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
	Cache     BaseCache `gorm:"-:all" json:"-"`
}

type BaseCache struct {
	URL rwmap.RWMap[string, *refreshcache.RefreshCache[string]]
	MPD atomic.Pointer[refreshcache.RefreshCache[*MPDCache]]
}

type MPDCache struct {
	MPDFile string
	URLs    []string
}

func (b *BaseCache) Clear() {
	b.MPD.Store(nil)
	b.URL.Clear()
}

func (b *BaseCache) InitOrLoadURLCache(id string, refreshFunc func() (string, error), maxAge time.Duration) (*refreshcache.RefreshCache[string], error) {
	c, loaded := b.URL.Load(id)
	if loaded {
		return c, nil
	}

	c, _ = b.URL.LoadOrStore(id, refreshcache.NewRefreshCache[string](refreshFunc, maxAge))
	return c, nil
}

func (b *BaseCache) InitOrLoadMPDCache(refreshFunc func() (*MPDCache, error), maxAge time.Duration) (*refreshcache.RefreshCache[*MPDCache], error) {
	c := b.MPD.Load()
	if c != nil {
		return c, nil
	}

	c = refreshcache.NewRefreshCache[*MPDCache](refreshFunc, maxAge)
	if b.MPD.CompareAndSwap(nil, c) {
		return c, nil
	} else {
		return b.InitOrLoadMPDCache(refreshFunc, maxAge)
	}
}

func (m *Movie) BeforeCreate(tx *gorm.DB) error {
	if m.ID == "" {
		m.ID = utils.SortUUID()
	}
	return nil
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

type VendorInfo struct {
	Vendor   StreamingVendor     `json:"vendor"`
	Shared   bool                `gorm:"not null;default:false" json:"shared"`
	Bilibili *BilibiliVendorInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
}

type BilibiliVendorInfo struct {
	Bvid       string `json:"bvid,omitempty"`
	Cid        uint64 `json:"cid,omitempty"`
	Epid       uint64 `json:"epid,omitempty"`
	Quality    uint64 `json:"quality,omitempty"`
	VendorName string `json:"vendorName,omitempty"`
}

func (b *BilibiliVendorInfo) Validate() error {
	if b.Bvid == "" && b.Epid == 0 {
		return fmt.Errorf("bvid and epid are empty")
	}

	if b.Bvid != "" && b.Epid != 0 {
		return fmt.Errorf("bvid and epid can't be set at the same time")
	}

	if b.Bvid != "" && b.Cid == 0 {
		return fmt.Errorf("cid is empty")
	}

	return nil
}
