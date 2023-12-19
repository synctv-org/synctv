package model

import (
	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type BilibiliVendor struct {
	UserID  string `gorm:"primaryKey"`
	Backend string
	Cookies map[string]string `gorm:"serializer:fastjson"`
}

func (b *BilibiliVendor) BeforeSave(tx *gorm.DB) error {
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

func (b *BilibiliVendor) AfterSave(tx *gorm.DB) error {
	key := []byte(b.UserID)
	for k, v := range b.Cookies {
		value, err := utils.DecryptoFromBase64(v, key)
		if err != nil {
			return err
		}
		b.Cookies[k] = string(value)
	}
	return nil
}

func (b *BilibiliVendor) AfterFind(tx *gorm.DB) error {
	return b.AfterSave(tx)
}

type AlistVendor struct {
	UserID         string `gorm:"primaryKey"`
	Backend        string
	Host           string
	Username       string
	HashedPassword []byte
}

func (a *AlistVendor) BeforeSave(tx *gorm.DB) error {
	key := []byte(a.UserID)
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

func (a *AlistVendor) AfterSave(tx *gorm.DB) error {
	key := []byte(a.UserID)
	if v, err := utils.DecryptoFromBase64(a.Host, key); err != nil {
		return err
	} else {
		a.Host = string(v)
	}
	if v, err := utils.DecryptoFromBase64(a.Username, key); err != nil {
		return err
	} else {
		a.Username = string(v)
	}
	if v, err := utils.Decrypto(a.HashedPassword, key); err != nil {
		return err
	} else {
		a.HashedPassword = v
	}
	return nil
}

func (a *AlistVendor) AfterFind(tx *gorm.DB) error {
	return a.AfterSave(tx)
}

type EmbyVendor struct {
	UserID  string `gorm:"primaryKey"`
	Backend string
	Host    string
	ApiKey  string
}

func (e *EmbyVendor) BeforeSave(tx *gorm.DB) error {
	key := []byte(e.UserID)
	var err error
	if e.Host, err = utils.CryptoToBase64([]byte(e.Host), key); err != nil {
		return err
	}
	if e.ApiKey, err = utils.CryptoToBase64([]byte(e.ApiKey), key); err != nil {
		return err
	}
	return nil
}

func (e *EmbyVendor) AfterSave(tx *gorm.DB) error {
	key := []byte(e.UserID)
	if v, err := utils.DecryptoFromBase64(e.Host, key); err != nil {
		return err
	} else {
		e.Host = string(v)
	}
	if v, err := utils.DecryptoFromBase64(e.ApiKey, key); err != nil {
		return err
	} else {
		e.ApiKey = string(v)
	}
	return nil
}

func (e *EmbyVendor) AfterFind(tx *gorm.DB) error {
	return e.AfterSave(tx)
}
