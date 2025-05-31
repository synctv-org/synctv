package model

import (
	"errors"
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"gorm.io/gorm"
)

type Consul struct {
	ServiceName string `gorm:"type:varchar(64)"  json:"serviceName"`
	Token       string `gorm:"type:varchar(256)" json:"token"`
	PathPrefix  string `gorm:"type:varchar(64)"  json:"pathPrefix"`
	Namespace   string `gorm:"type:varchar(64)"  json:"namespace"`
	Partition   string `gorm:"type:varchar(64)"  json:"partition"`
}

type Etcd struct {
	ServiceName string `gorm:"type:varchar(64)"  json:"serviceName"`
	Username    string `gorm:"type:varchar(64)"  json:"username"`
	Password    string `gorm:"type:varchar(256)" json:"password"`
}

type Backend struct {
	Consul    Consul `gorm:"embedded;embeddedPrefix:consul_" json:"consul"`
	Etcd      Etcd   `gorm:"embedded;embeddedPrefix:etcd_"   json:"etcd"`
	Endpoint  string `gorm:"primaryKey;type:varchar(512)"    json:"endpoint"`
	Comment   string `gorm:"type:text"                       json:"comment"`
	JwtSecret string `gorm:"type:varchar(256)"               json:"jwtSecret"`
	CustomCa  string `gorm:"type:text"                       json:"customCa"`
	TimeOut   string `gorm:"default:10s"                     json:"timeOut"`
	TLS       bool   `gorm:"default:false"                   json:"tls"`
}

func (b *Backend) Validate() error {
	if b.Endpoint == "" {
		return errors.New("new http client failed, endpoint is empty")
	}
	if b.Consul.ServiceName != "" && b.Etcd.ServiceName != "" {
		return errors.New("new grpc client failed, consul and etcd can't be used at the same time")
	}
	if b.TimeOut != "" {
		if _, err := time.ParseDuration(b.TimeOut); err != nil {
			return err
		}
	}
	return nil
}

type VendorBackend struct {
	CreatedAt time.Time
	UpdatedAt time.Time
	UsedBy    BackendUsedBy `gorm:"embedded;embeddedPrefix:used_by_" json:"usedBy"`
	Backend   Backend       `gorm:"embedded;embeddedPrefix:backend_" json:"backend"`
}

type BackendUsedBy struct {
	BilibiliBackendName string `gorm:"type:varchar(64)" json:"bilibiliBackendName"`
	AlistBackendName    string `gorm:"type:varchar(64)" json:"alistBackendName"`
	EmbyBackendName     string `gorm:"type:varchar(64)" json:"embyBackendName"`
	Enabled             bool   `gorm:"default:false"    json:"enabled"`
	Bilibili            bool   `gorm:"default:false"    json:"bilibili"`
	Alist               bool   `gorm:"default:false"    json:"alist"`
	Emby                bool   `gorm:"default:false"    json:"emby"`
}

func (v *VendorBackend) BeforeSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(v.Backend.Endpoint)
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
	if v.Backend.CustomCa != "" {
		if v.Backend.CustomCa, err = utils.CryptoToBase64([]byte(v.Backend.CustomCa), key); err != nil {
			return err
		}
	}
	return nil
}

func (v *VendorBackend) AfterSave(_ *gorm.DB) error {
	key := utils.GenCryptoKey(v.Backend.Endpoint)
	if v.Backend.JwtSecret != "" {
		jwtSecret, err := utils.DecryptoFromBase64(v.Backend.JwtSecret, key)
		if err != nil {
			return err
		}
		v.Backend.JwtSecret = stream.BytesToString(jwtSecret)
	}
	if v.Backend.Consul.Token != "" {
		token, err := utils.DecryptoFromBase64(v.Backend.Consul.Token, key)
		if err != nil {
			return err
		}
		v.Backend.Consul.Token = stream.BytesToString(token)
	}
	if v.Backend.Etcd.Password != "" {
		password, err := utils.DecryptoFromBase64(v.Backend.Etcd.Password, key)
		if err != nil {
			return err
		}
		v.Backend.Etcd.Password = stream.BytesToString(password)
	}
	if v.Backend.CustomCa != "" {
		customCa, err := utils.DecryptoFromBase64(v.Backend.CustomCa, key)
		if err != nil {
			return err
		}
		v.Backend.CustomCa = stream.BytesToString(customCa)
	}
	return nil
}

func (v *VendorBackend) AfterFind(tx *gorm.DB) error {
	return v.AfterSave(tx)
}
