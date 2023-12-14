package model

import (
	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

type Consul struct {
	ServerName string
	Token      string
	TokenFile  string
	PathPrefix string
	Namespace  string
	Partition  string
}

type Etcd struct {
	ServerName string
	Username   string
	Password   string
}

type Backend struct {
	Endpoint     string `gorm:"primaryKey" json:"endpoint"`
	Comment      string `gorm:"type:text" json:"comment"`
	Tls          bool   `gorm:"default:false" json:"tls"`
	JwtSecret    string `json:"jwtSecret"`
	CustomCAFile string `json:"customCaFile"`
	TimeOut      string `gorm:"default:10s" json:"timeOut"`

	Consul Consul `gorm:"embedded;embeddedPrefix:consul_" json:"consul"`
	Etcd   Etcd   `gorm:"embedded;embeddedPrefix:etcd_" json:"etcd"`
}

type VendorBackend struct {
	Backend Backend       `gorm:"embedded;embeddedPrefix:backend_" json:"backend"`
	UsedBy  BackendUsedBy `gorm:"embedded;embeddedPrefix:used_by_" json:"usedBy"`
}

type BackendUsedBy struct {
	Bilibili            bool   `gorm:"default:false" json:"bilibili"`
	BilibiliBackendName string `json:"bilibiliBackendName"`
	Alist               bool   `gorm:"default:false" json:"alist"`
	AlistBackendName    string `json:"alistBackendName"`
	Emby                bool   `gorm:"default:false" json:"emby"`
	EmbyBackendName     string `json:"embyBackendName"`
}

func (v *VendorBackend) BeforeSave(tx *gorm.DB) error {
	key := []byte(v.Backend.Endpoint)
	var err error
	if v.Backend.JwtSecret != "" {
		if v.Backend.JwtSecret, err = utils.CryptoToBase64([]byte(v.Backend.JwtSecret), key); err != nil {
			return err
		}
	}
	if v.Backend.Consul.Token != "" {
		if v.Backend.Consul.Token, err = utils.CryptoToBase64([]byte(v.Backend.Consul.Token), key); err != nil {
			return err
		}
	}
	if v.Backend.Etcd.Password != "" {
		if v.Backend.Etcd.Password, err = utils.CryptoToBase64([]byte(v.Backend.Etcd.Password), key); err != nil {
			return err
		}
	}
	return nil
}

func (v *VendorBackend) AfterFind(tx *gorm.DB) error {
	key := []byte(v.Backend.Endpoint)
	var (
		err  error
		data []byte
	)
	if v.Backend.JwtSecret != "" {
		if data, err = utils.DecryptoFromBase64(v.Backend.JwtSecret, key); err != nil {
			return err
		} else {
			v.Backend.JwtSecret = string(data)
		}
	}
	if v.Backend.Consul.Token != "" {
		if data, err = utils.DecryptoFromBase64(v.Backend.Consul.Token, key); err != nil {
			return err
		} else {
			v.Backend.Consul.Token = string(data)
		}
	}
	if v.Backend.Etcd.Password != "" {
		if data, err = utils.DecryptoFromBase64(v.Backend.Etcd.Password, key); err != nil {
			return err
		} else {
			v.Backend.Etcd.Password = string(data)
		}
	}
	return nil
}