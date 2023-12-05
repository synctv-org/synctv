package db

import (
	"errors"
	"net/http"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func GetVendorByUserID(userID string) ([]*model.StreamingVendorInfo, error) {
	var vendors []*model.StreamingVendorInfo
	err := db.Where("user_id = ?", userID).Find(&vendors).Error
	if err != nil {
		return nil, err
	}
	return vendors, nil
}

func GetVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor) (*model.StreamingVendorInfo, error) {
	var vendorInfo model.StreamingVendorInfo
	err := db.Where("user_id = ? AND vendor = ?", userID, vendor).First(&vendorInfo).Error
	return &vendorInfo, HandleNotFound(err, "vendor")
}

type CreateVendorConfig func(*model.StreamingVendorInfo)

func WithCookie(cookie []*http.Cookie) CreateVendorConfig {
	return func(vendor *model.StreamingVendorInfo) {
		vendor.Cookies = cookie
	}
}

func WithAuthorization(authorization string) CreateVendorConfig {
	return func(vendor *model.StreamingVendorInfo) {
		vendor.Authorization = authorization
	}
}

func WithPassword(password string) CreateVendorConfig {
	return func(vendor *model.StreamingVendorInfo) {
		vendor.Password = password
	}
}

func WithHost(host string) CreateVendorConfig {
	return func(vendor *model.StreamingVendorInfo) {
		vendor.Host = host
	}
}

func FirstOrCreateVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor, conf ...CreateVendorConfig) (*model.StreamingVendorInfo, error) {
	var vendorInfo model.StreamingVendorInfo
	v := &model.StreamingVendorInfo{
		UserID: userID,
		Vendor: vendor,
	}
	for _, c := range conf {
		c(v)
	}
	err := db.Where("user_id = ? AND vendor = ?", userID, vendor).Attrs(
		v,
	).FirstOrCreate(&vendorInfo).Error
	return &vendorInfo, err
}

func CreateOrSaveVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor, conf ...CreateVendorConfig) (*model.StreamingVendorInfo, error) {
	vendorInfo := model.StreamingVendorInfo{
		UserID: userID,
		Vendor: vendor,
	}
	return &vendorInfo, Transactional(func(tx *gorm.DB) error {
		if errors.Is(tx.First(&vendorInfo).Error, gorm.ErrRecordNotFound) {
			for _, c := range conf {
				c(&vendorInfo)
			}
			return tx.Create(&vendorInfo).Error
		} else {
			for _, c := range conf {
				c(&vendorInfo)
			}
			return tx.Save(&vendorInfo).Error
		}
	})
}

func DeleteVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor) error {
	return db.Where("user_id = ? AND vendor = ?", userID, vendor).Delete(&model.StreamingVendorInfo{}).Error
}
