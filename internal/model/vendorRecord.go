package model

import (
	"time"

	"github.com/google/uuid"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"gorm.io/gorm"
)

type BilibiliVendor struct {
	CreatedAt time.Time
	UpdatedAt time.Time
	Cookies   map[string]string `gorm:"not null;serializer:fastjson;type:text"`
	UserID    string            `gorm:"primaryKey;type:char(32)"`
	Backend   string            `gorm:"type:varchar(64)"`
}

func (b *BilibiliVendor) BeforeSave(_ *gorm.DB) error {
	key := []byte(b.UserID)
	for k, v := range b.Cookies {
		value, err := utils.CryptoToBase64([]byte(v), key)
		if err != nil {
			return err
		}
		b.Cookies[k] = value
	}
	return nil
}

func (b *BilibiliVendor) AfterSave(_ *gorm.DB) error {
	key := []byte(b.UserID)
	for k, v := range b.Cookies {
		value, err := utils.DecryptoFromBase64(v, key)
		if err != nil {
			return err
		}
		b.Cookies[k] = stream.BytesToString(value)
	}
	return nil
}

func (b *BilibiliVendor) AfterFind(tx *gorm.DB) error {
	return b.AfterSave(tx)
}

type AlistVendor struct {
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         string `gorm:"primaryKey;type:char(32)"`
	Backend        string `gorm:"type:varchar(64)"`
	ServerID       string `gorm:"primaryKey;type:char(32)"`
	Host           string `gorm:"not null;type:varchar(256)"`
	Username       string `gorm:"type:varchar(256)"`
	HashedPassword []byte
}

func GenAlistServerID(a *AlistVendor) {
	if a.ServerID == "" {
		a.ServerID = utils.SortUUIDWithUUID(uuid.NewMD5(uuid.NameSpaceURL, []byte(a.Host)))
	}
}

func (a *AlistVendor) BeforeSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(a.UserID)
	var err error
	if a.Host, err = utils.CryptoToBase64([]byte(a.Host), key); err != nil {
		return err
	}
	if a.Username, err = utils.CryptoToBase64([]byte(a.Username), key); err != nil {
		return err
	}
	if a.HashedPassword, err = utils.Crypto(a.HashedPassword, key); err != nil {
		return err
	}
	return nil
}

func (a *AlistVendor) AfterSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(a.UserID)
	host, err := utils.DecryptoFromBase64(a.Host, key)
	if err != nil {
		return err
	}
	a.Host = stream.BytesToString(host)
	username, err := utils.DecryptoFromBase64(a.Username, key)
	if err != nil {
		return err
	}
	a.Username = stream.BytesToString(username)
	hashedPassword, err := utils.Decrypto(a.HashedPassword, key)
	if err != nil {
		return err
	}
	a.HashedPassword = hashedPassword
	return nil
}

func (a *AlistVendor) AfterFind(tx *gorm.DB) error {
	return a.AfterSave(tx)
}

type EmbyVendor struct {
	CreatedAt  time.Time
	UpdatedAt  time.Time
	UserID     string `gorm:"primaryKey;type:char(32)"`
	Backend    string `gorm:"type:varchar(64)"`
	ServerID   string `gorm:"primaryKey;type:char(32)"`
	Host       string `gorm:"not null;type:varchar(256)"`
	APIKey     string `gorm:"not null;type:varchar(256)"`
	EmbyUserID string `gorm:"type:varchar(32)"`
}

func (e *EmbyVendor) BeforeSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(e.ServerID)
	var err error
	if e.Host, err = utils.CryptoToBase64(stream.StringToBytes(e.Host), key); err != nil {
		return err
	}
	if e.APIKey, err = utils.CryptoToBase64(stream.StringToBytes(e.APIKey), key); err != nil {
		return err
	}
	return nil
}

func (e *EmbyVendor) AfterSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(e.ServerID)
	host, err := utils.DecryptoFromBase64(e.Host, key)
	if err != nil {
		return err
	}
	e.Host = stream.BytesToString(host)
	apiKey, err := utils.DecryptoFromBase64(e.APIKey, key)
	if err != nil {
		return err
	}
	e.APIKey = stream.BytesToString(apiKey)
	return nil
}

func (e *EmbyVendor) AfterFind(tx *gorm.DB) error {
	return e.AfterSave(tx)
}
