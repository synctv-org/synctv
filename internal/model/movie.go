package model

import (
	"time"

	"github.com/google/uuid"
	"gorm.io/gorm"
)

type Movie struct {
	ID        string    `gorm:"primaryKey;type:varchar(36)" json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	Position  uint      `gorm:"not null" json:"-"`
	RoomID    string    `gorm:"not null;index" json:"-"`
	CreatorID string    `gorm:"not null;index" json:"creatorId"`
	Base      BaseMovie `gorm:"embedded;embeddedPrefix:base_" json:"base"`
}

func (m *Movie) BeforeCreate(tx *gorm.DB) error {
	if m.ID == "" {
		m.ID = uuid.NewString()
	}
	return nil
}

type BaseMovie struct {
	Url        string            `json:"url,omitempty"`
	Name       string            `gorm:"not null" json:"name"`
	Live       bool              `json:"live,omitempty"`
	Proxy      bool              `json:"proxy,omitempty"`
	RtmpSource bool              `json:"rtmpSource,omitempty"`
	Type       string            `json:"type,omitempty"`
	Headers    map[string]string `gorm:"serializer:fastjson" json:"headers,omitempty"`
	VendorInfo `gorm:"embedded;embeddedPrefix:vendor_info_" json:"vendorInfo,omitempty"`
}

type VendorInfo struct {
	Vendor             StreamingVendor    `json:"vendor"`
	Shared             bool               `gorm:"not null;default:false" json:"shared"`
	BilibiliVendorInfo BilibiliVendorInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibiliVendorInfo,omitempty"`
}

type BilibiliVendorInfo struct {
	Bvid    string `json:"bvid,omitempty"`
	Cid     uint   `json:"cid,omitempty"`
	Epid    uint   `json:"epid,omitempty"`
	Quality uint   `json:"quality,omitempty"`
}
