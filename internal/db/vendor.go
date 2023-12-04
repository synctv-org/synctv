package db

import (
	"net/http"

	"github.com/synctv-org/synctv/internal/model"
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

func AssignFirstOrCreateVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor, conf ...CreateVendorConfig) (*model.StreamingVendorInfo, error) {
	var vendorInfo model.StreamingVendorInfo
	v := &model.StreamingVendorInfo{
		UserID: userID,
		Vendor: vendor,
	}
	for _, c := range conf {
		c(v)
	}
	err := db.Where("user_id = ? AND vendor = ?", userID, vendor).Assign(
		v,
	).FirstOrCreate(&vendorInfo).Error
	return &vendorInfo, err
}

func FirstOrInitVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor, conf ...CreateVendorConfig) (*model.StreamingVendorInfo, error) {
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
	).FirstOrInit(&vendorInfo).Error
	return &vendorInfo, err
}

func DeleteVendorByUserIDAndVendor(userID string, vendor model.StreamingVendor) error {
	return db.Where("user_id = ? AND vendor = ?", userID, vendor).Delete(&model.StreamingVendorInfo{}).Error
}
