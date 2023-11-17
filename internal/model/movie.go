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
