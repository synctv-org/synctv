package model

import (
	"fmt"
	"time"

	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type Movie struct {
	ID        string    `gorm:"primaryKey;type:varchar(32)" json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	Position  uint      `gorm:"not null" json:"-"`
	RoomID    string    `gorm:"not null;index" json:"-"`
	CreatorID string    `gorm:"index" json:"creatorId"`
	Base      BaseMovie `gorm:"embedded;embeddedPrefix:base_" json:"base"`
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
	Backend  string              `json:"backend"`
	Shared   bool                `gorm:"not null;default:false" json:"shared"`
	Bilibili *BilibiliVendorInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
	Alist    *AlistVendorInfo    `gorm:"embedded;embeddedPrefix:alist_" json:"alist,omitempty"`
}

type BilibiliVendorInfo struct {
	Bvid    string `json:"bvid,omitempty"`
	Cid     uint64 `json:"cid,omitempty"`
	Epid    uint64 `json:"epid,omitempty"`
	Quality uint64 `json:"quality,omitempty"`
}

func (b *BilibiliVendorInfo) Validate() error {
	switch {
	// 先判断epid是否为0来确定是否是pgc
	case b.Epid != 0:
		if b.Bvid == "" || b.Cid == 0 {
			return fmt.Errorf("bvid or cid is empty")
		}
	case b.Bvid != "":
		if b.Cid == 0 {
			return fmt.Errorf("cid is empty")
		}
	default:
		return fmt.Errorf("bvid or epid is empty")
	}

	return nil
}

type AlistVendorInfo struct {
	Path     string `json:"path,omitempty"`
	Password string `json:"password,omitempty"`
}
