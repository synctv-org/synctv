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
	Url        string               `json:"url"`
	Name       string               `gorm:"not null" json:"name"`
	Live       bool                 `json:"live"`
	Proxy      bool                 `json:"proxy"`
	RtmpSource bool                 `json:"rtmpSource"`
	Type       string               `json:"type"`
	Headers    map[string]string    `gorm:"serializer:fastjson" json:"headers"`
	Subtitles  map[string]*Subtitle `gorm:"serializer:fastjson" json:"subtitles"`
	VendorInfo VendorInfo           `gorm:"embedded;embeddedPrefix:vendor_info_" json:"vendorInfo,omitempty"`
}

type Subtitle struct {
	URL  string `json:"url"`
	Type string `json:"type"`
}

type VendorName = string

const (
	VendorBilibili VendorName = "bilibili"
	VendorAlist    VendorName = "alist"
)

type VendorInfo struct {
	Vendor   VendorName             `json:"vendor"`
	Backend  string                 `json:"backend"`
	Shared   bool                   `gorm:"not null;default:false" json:"shared"`
	Bilibili *BilibiliStreamingInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
	Alist    *AlistStreamingInfo    `gorm:"embedded;embeddedPrefix:alist_" json:"alist,omitempty"`
}

type BilibiliStreamingInfo struct {
	Bvid    string `json:"bvid,omitempty"`
	Cid     uint64 `json:"cid,omitempty"`
	Epid    uint64 `json:"epid,omitempty"`
	Quality uint64 `json:"quality,omitempty"`
}

func (b *BilibiliStreamingInfo) Validate() error {
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

type AlistStreamingInfo struct {
	Path     string `json:"path,omitempty"`
	Password string `json:"password,omitempty"`
}

func (a *AlistStreamingInfo) BeforeSave(tx *gorm.DB) error {
	if a.Password != "" {
		s, err := utils.CryptoToBase64([]byte(a.Password), utils.GenCryptoKey(a.Path))
		if err != nil {
			return err
		}
		a.Password = s
	}
	return nil
}

func (a *AlistStreamingInfo) AfterFind(tx *gorm.DB) error {
	if a.Password != "" {
		b, err := utils.DecryptoFromBase64(a.Password, utils.GenCryptoKey(a.Path))
		if err != nil {
			return err
		}
		a.Password = string(b)
	}
	return nil
}
