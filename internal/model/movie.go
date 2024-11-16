package model

import (
	"database/sql/driver"
	"errors"
	"fmt"
	"strings"
	"time"

	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type Movie struct {
	ID        string    `gorm:"primaryKey;type:char(32)"                                         json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	RoomID    string    `gorm:"not null;index;type:char(32)"                                     json:"-"`
	CreatorID string    `gorm:"index;type:char(32)"                                              json:"creatorId"`
	Childrens []*Movie  `gorm:"foreignKey:ParentID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE" json:"-"`
	MovieBase `gorm:"embedded;embeddedPrefix:base_"                                    json:"base"`
	Position  uint `gorm:"not null"                                                         json:"-"`
}

func (m *Movie) Clone() *Movie {
	return &Movie{
		ID:        m.ID,
		CreatedAt: m.CreatedAt,
		UpdatedAt: m.UpdatedAt,
		Position:  m.Position,
		RoomID:    m.RoomID,
		CreatorID: m.CreatorID,
		MovieBase: *m.MovieBase.Clone(),
		Childrens: m.Childrens,
	}
}

func (m *Movie) BeforeCreate(tx *gorm.DB) error {
	if m.ID == "" {
		m.ID = utils.SortUUID()
	}
	return nil
}

func (m *Movie) BeforeSave(tx *gorm.DB) (err error) {
	if m.ParentID != "" {
		mv := &Movie{}
		err = tx.Where("id = ?", m.ParentID).First(mv).Error
		if err != nil {
			return fmt.Errorf("load parent movie failed: %w", err)
		}
		if !mv.IsFolder {
			return errors.New("parent is not a folder")
		}
		if mv.IsDynamicFolder() {
			return errors.New("parent is a dynamic folder, cannot add child")
		}
	}
	return
}

type MoreSource struct {
	Name string `json:"name"`
	Type string `json:"type"`
	URL  string `json:"url"`
}

type MovieBase struct {
	VendorInfo  VendorInfo           `gorm:"embedded;embeddedPrefix:vendor_info_" json:"vendorInfo,omitempty"`
	Headers     map[string]string    `gorm:"serializer:fastjson;type:text"        json:"headers,omitempty"`
	Subtitles   map[string]*Subtitle `gorm:"serializer:fastjson;type:text"        json:"subtitles,omitempty"`
	URL         string               `gorm:"type:varchar(8192)"                   json:"url"`
	Name        string               `gorm:"not null;type:varchar(256)"           json:"name"`
	Type        string               `json:"type"`
	ParentID    EmptyNullString      `gorm:"type:char(32)"                        json:"parentId"`
	MoreSources []*MoreSource        `gorm:"serializer:fastjson;type:text"        json:"moreSources,omitempty"`
	Live        bool                 `json:"live"`
	Proxy       bool                 `json:"proxy"`
	RtmpSource  bool                 `json:"rtmpSource"`
	IsFolder    bool                 `json:"isFolder"`
}

func (m *MovieBase) IsM3u8() bool {
	return strings.HasPrefix(m.Type, "m3u") || utils.IsM3u8Url(m.URL)
}

func (m *MovieBase) Clone() *MovieBase {
	mss := make([]*MoreSource, len(m.MoreSources))
	for i, ms := range m.MoreSources {
		mss[i] = &MoreSource{
			Name: ms.Name,
			Type: ms.Type,
			URL:  ms.URL,
		}
	}
	hds := make(map[string]string, len(m.Headers))
	for k, v := range m.Headers {
		hds[k] = v
	}
	sbs := make(map[string]*Subtitle, len(m.Subtitles))
	for k, v := range m.Subtitles {
		sbs[k] = &Subtitle{
			URL:  v.URL,
			Type: v.Type,
		}
	}
	return &MovieBase{
		URL:         m.URL,
		MoreSources: mss,
		Name:        m.Name,
		Live:        m.Live,
		Proxy:       m.Proxy,
		RtmpSource:  m.RtmpSource,
		Type:        m.Type,
		Headers:     hds,
		Subtitles:   sbs,
		VendorInfo:  m.VendorInfo,
		IsFolder:    m.IsFolder,
		ParentID:    m.ParentID,
	}
}

func (m *MovieBase) IsDynamicFolder() bool {
	return m.IsFolder && m.VendorInfo.Vendor != ""
}

type EmptyNullString string

func (ns EmptyNullString) String() string {
	return string(ns)
}

// Scan implements the [Scanner] interface.
func (ns *EmptyNullString) Scan(value any) error {
	if value == nil {
		*ns = ""
		return nil
	}
	switch v := value.(type) {
	case []byte:
		*ns = EmptyNullString(v)
	case string:
		*ns = EmptyNullString(v)
	default:
		return fmt.Errorf("unsupported type: %T", v)
	}
	return nil
}

// Value implements the [driver.Valuer] interface.
func (ns EmptyNullString) Value() (driver.Value, error) {
	if ns == "" {
		return nil, nil
	}
	return string(ns), nil
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
	Bilibili *BilibiliStreamingInfo `gorm:"embedded;embeddedPrefix:bilibili_" json:"bilibili,omitempty"`
	Alist    *AlistStreamingInfo    `gorm:"embedded;embeddedPrefix:alist_"    json:"alist,omitempty"`
	Emby     *EmbyStreamingInfo     `gorm:"embedded;embeddedPrefix:emby_"     json:"emby,omitempty"`
	Vendor   VendorName             `gorm:"type:varchar(32)"                  json:"vendor"`
	Backend  string                 `gorm:"type:varchar(64)"                  json:"backend"`
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
			return errors.New("bvid or cid is empty")
		}
	case b.Bvid != "":
		if b.Cid == 0 {
			return errors.New("cid is empty")
		}
	case b.Cid != 0: // live
		return nil
	default:
		return errors.New("bvid or epid is empty")
	}

	return nil
}

type AlistStreamingInfo struct {
	// {/}serverId/Path
	Path     string `gorm:"type:varchar(4096)" json:"path,omitempty"`
	Password string `gorm:"type:varchar(256)"  json:"password,omitempty"`
}

func GetAlistServerIDFromPath(path string) (serverID string, filePath string, err error) {
	before, after, found := strings.Cut(strings.TrimLeft(path, "/"), "/")
	if !found {
		return "", path, errors.New("path is invalid")
	}
	return before, after, nil
}

func FormatAlistPath(serverID, filePath string) string {
	return fmt.Sprintf("%s/%s", serverID, strings.Trim(filePath, "/"))
}

func (a *AlistStreamingInfo) SetServerIDAndFilePath(serverID, filePath string) {
	a.Path = FormatAlistPath(serverID, filePath)
}

func (a *AlistStreamingInfo) ServerID() (string, error) {
	serverID, _, err := GetAlistServerIDFromPath(a.Path)
	return serverID, err
}

func (a *AlistStreamingInfo) FilePath() (string, error) {
	_, filePath, err := GetAlistServerIDFromPath(a.Path)
	return filePath, err
}

func (a *AlistStreamingInfo) ServerIDAndFilePath() (serverID, filePath string, err error) {
	return GetAlistServerIDFromPath(a.Path)
}

func (a *AlistStreamingInfo) Validate() error {
	if a.Path == "" {
		return errors.New("path is empty")
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
	Path      string `gorm:"type:varchar(52)"    json:"path,omitempty"`
	Transcode bool   `json:"transcode,omitempty"`
}

func GetEmbyServerIDFromPath(path string) (serverID string, filePath string, err error) {
	if s := strings.Split(strings.TrimLeft(path, "/"), "/"); len(s) == 2 {
		return s[0], s[1], nil
	}
	return "", path, errors.New("path is invalid")
}

func FormatEmbyPath(serverID, filePath string) string {
	return fmt.Sprintf("%s/%s", serverID, filePath)
}

func (e *EmbyStreamingInfo) SetServerIDAndFilePath(serverID, filePath string) {
	e.Path = FormatEmbyPath(serverID, filePath)
}

func (e *EmbyStreamingInfo) ServerID() (string, error) {
	serverID, _, err := GetEmbyServerIDFromPath(e.Path)
	return serverID, err
}

func (e *EmbyStreamingInfo) FilePath() (string, error) {
	_, filePath, err := GetEmbyServerIDFromPath(e.Path)
	return filePath, err
}

func (e *EmbyStreamingInfo) ServerIDAndFilePath() (serverID, filePath string, err error) {
	return GetEmbyServerIDFromPath(e.Path)
}

func (e *EmbyStreamingInfo) Validate() error {
	if e.Path == "" {
		return errors.New("path is empty")
	}
	return nil
}
