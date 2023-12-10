package model

import (
	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type BilibiliVendor struct {
	UserID  string            `gorm:"primaryKey"`
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

func (b *BilibiliVendor) AfterFind(tx *gorm.DB) error {
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

type AlistVendor struct {
	UserID   string `gorm:"primaryKey"`
	Host     string `gorm:"serializer:fastjson"`
	Username string `gorm:"serializer:fastjson"`
	Password string `gorm:"serializer:fastjson"`
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
	if a.Password, err = utils.CryptoToBase64([]byte(a.Password), key); err != nil {
		return err
	}
	return nil
}

func (a *AlistVendor) AfterFind(tx *gorm.DB) error {
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
	if v, err := utils.DecryptoFromBase64(a.Password, key); err != nil {
		return err
	} else {
		a.Password = string(v)
	}
	return nil
}
