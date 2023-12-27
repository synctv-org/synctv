package model

import (
	"fmt"
	"strings"
	"time"

	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type Movie struct {
	ID        string    `gorm:"primaryKey;type:char(32)" json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	Position  uint      `gorm:"not null" json:"-"`
	RoomID    string    `gorm:"not null;index;type:char(32)" json:"-"`
	CreatorID string    `gorm:"index;type:char(32)" json:"creatorId"`
	Base      BaseMovie `gorm:"embedded;embeddedPrefix:base_" json:"base"`
}

func (m *Movie) BeforeCreate(tx *gorm.DB) error {
	if m.ID == "" {
		m.ID = utils.SortUUID()
	}
	return nil
}

type BaseMovie struct {
	Url        string               `gorm:"type:varchar(8192)" json:"url"`
	Name       string               `gorm:"not null;type:varchar(128)" json:"name"`
	Live       bool                 `json:"live"`
	Proxy      bool                 `json:"proxy"`
	RtmpSource bool                 `json:"rtmpSource"`
	Type       string               `json:"type"`
	Headers    map[string]string    `gorm:"serializer:fastjson;type:text" json:"headers"`
	Subtitles  map[string]*Subtitle `gorm:"serializer:fastjson;type:text" json:"subtitles"`
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
	VendorEmby     VendorName = "emby"
)

type VendorInfo struct {
	Vendor   VendorName             `gorm:"type:varchar(32)" json:"vendor"`
	Backend  string                 `gorm:"type:varchar(64)" json:"backend"`
	Bilibili *BilibiliStreamingInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
	Alist    *AlistStreamingInfo    `gorm:"embedded;embeddedPrefix:alist_" json:"alist,omitempty"`
	Emby     *EmbyStreamingInfo     `gorm:"embedded;embeddedPrefix:emby_" json:"emby,omitempty"`
}

type BilibiliStreamingInfo struct {
	Bvid    string `json:"bvid,omitempty"`
	Cid     uint64 `json:"cid,omitempty"`
	Epid    uint64 `json:"epid,omitempty"`
	Quality uint64 `json:"quality,omitempty"`
	Shared  bool   `json:"shared,omitempty"`
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
	// {/}serverId/Path
	Path     string `gorm:"type:varchar(4096)" json:"path,omitempty"`
	Password string `gorm:"type:varchar(256)" json:"password,omitempty"`
}

func GetAlistServerIdFromPath(path string) (serverID string, filePath string, err error) {
	before, after, found := strings.Cut(strings.TrimLeft(path, "/"), "/")
	if !found {
		return "", path, fmt.Errorf("path is invalid")
	}
	return before, after, nil
}

func (a *AlistStreamingInfo) Validate() error {
	if a.Path == "" {
		return fmt.Errorf("path is empty")
	}
	return nil
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

func (a *AlistStreamingInfo) AfterSave(tx *gorm.DB) error {
	if a.Password != "" {
		b, err := utils.DecryptoFromBase64(a.Password, utils.GenCryptoKey(a.Path))
		if err != nil {
			return err
		}
		a.Password = string(b)
	}
	return nil
}

func (a *AlistStreamingInfo) AfterFind(tx *gorm.DB) error {
	return a.AfterSave(tx)
}

type EmbyStreamingInfo struct {
	// {/}serverId/ItemId
	Path string `gorm:"type:varchar(52)" json:"path,omitempty"`
}

func GetEmbyServerIdFromPath(path string) (serverID string, filePath string, err error) {
	if s := strings.Split(strings.TrimLeft(path, "/"), "/"); len(s) == 2 {
		return s[0], s[1], nil
	}
	return "", path, fmt.Errorf("path is invalid")
}

func (e *EmbyStreamingInfo) Validate() error {
	if e.Path == "" {
		return fmt.Errorf("path is empty")
	}
	return nil
}
