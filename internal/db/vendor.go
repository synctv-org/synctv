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
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, errors.New("vendor not found")
	}
	return &vendorInfo, err
}

type CreateVendorConfig func(*model.StreamingVendorInfo)

func WithCookie(cookie []*http.Cookie) CreateVendorConfig {
	return func(vendor *model.StreamingVendorInfo) {
		vendor.Cookies = cookie
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
