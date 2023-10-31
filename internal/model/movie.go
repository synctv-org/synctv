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
	Url        string            `json:"url"`
	Name       string            `gorm:"not null" json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `json:"type"`
	Headers    map[string]string `gorm:"serializer:fastjson" json:"headers"`
	VendorInfo `gorm:"embedded;embeddedPrefix:vendor_info_" json:"vendorInfo,omitempty"`
}

type VendorInfo struct {
	Vendor   StreamingVendor    `json:"vendor"`
	Shared   bool               `gorm:"not null;default:false" json:"shared"`
	Bilibili BilibiliVendorInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
}

type BilibiliVendorInfo struct {
	Bvid    string `json:"bvid"`
	Cid     uint   `json:"cid"`
	Epid    uint   `json:"epid"`
	Quality uint   `json:"quality"`
}
