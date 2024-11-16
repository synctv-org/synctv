package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

const (
	ErrVendorNotFound = "vendor"
)

func GetBilibiliVendor(userID string) (*model.BilibiliVendor, error) {
	var vendor model.BilibiliVendor
	err := db.Where("user_id = ?", userID).First(&vendor).Error
	return &vendor, HandleNotFound(err, ErrVendorNotFound)
}

func CreateOrSaveBilibiliVendor(vendorInfo *model.BilibiliVendor) (*model.BilibiliVendor, error) {
	if vendorInfo.UserID == "" {
		return nil, errors.New("user_id must not be empty")
	}
	return vendorInfo, Transactional(func(tx *gorm.DB) error {
		if errors.Is(tx.First(&model.BilibiliVendor{
			UserID: vendorInfo.UserID,
		}).Error, gorm.ErrRecordNotFound) {
			return tx.Create(&vendorInfo).Error
		}
		result := tx.Omit("created_at").Save(&vendorInfo)
		return HandleUpdateResult(result, ErrVendorNotFound)
	})
}

func DeleteBilibiliVendor(userID string) error {
	result := db.Where("user_id = ?", userID).Delete(&model.BilibiliVendor{})
	return HandleUpdateResult(result, ErrVendorNotFound)
}

func GetAlistVendors(userID string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.AlistVendor, error) {
	var vendors []*model.AlistVendor
	err := db.Scopes(scopes...).Where("user_id = ?", userID).Find(&vendors).Error
	return vendors, err
}

func GetAlistVendorsCount(userID string, scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64
	err := db.Scopes(scopes...).Where("user_id = ?", userID).Model(&model.AlistVendor{}).Count(&count).Error
	return count, err
}

func GetAlistVendor(userID, serverID string) (*model.AlistVendor, error) {
	var vendor model.AlistVendor
	err := db.Where("user_id = ? AND server_id = ?", userID, serverID).First(&vendor).Error
	return &vendor, HandleNotFound(err, ErrVendorNotFound)
}

func CreateOrSaveAlistVendor(vendorInfo *model.AlistVendor) (*model.AlistVendor, error) {
	if vendorInfo.UserID == "" || vendorInfo.ServerID == "" {
		return nil, errors.New("user_id and server_id must not be empty")
	}
	return vendorInfo, Transactional(func(tx *gorm.DB) error {
		if errors.Is(tx.First(&model.AlistVendor{
			UserID:   vendorInfo.UserID,
			ServerID: vendorInfo.ServerID,
		}).Error, gorm.ErrRecordNotFound) {
			return tx.Create(&vendorInfo).Error
		}
		result := tx.Omit("created_at").Save(&vendorInfo)
		return HandleUpdateResult(result, ErrVendorNotFound)
	})
}

func DeleteAlistVendor(userID, serverID string) error {
	result := db.Where("user_id = ? AND server_id = ?", userID, serverID).Delete(&model.AlistVendor{})
	return HandleUpdateResult(result, ErrVendorNotFound)
}

func GetEmbyVendors(userID string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.EmbyVendor, error) {
	var vendors []*model.EmbyVendor
	err := db.Scopes(scopes...).Where("user_id = ?", userID).Find(&vendors).Error
	return vendors, err
}

func GetEmbyVendorsCount(userID string, scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64
	err := db.Scopes(scopes...).Where("user_id = ?", userID).Model(&model.EmbyVendor{}).Count(&count).Error
	return count, err
}

func GetEmbyVendor(userID, serverID string) (*model.EmbyVendor, error) {
	var vendor model.EmbyVendor
	err := db.Where("user_id = ? AND server_id = ?", userID, serverID).First(&vendor).Error
	return &vendor, HandleNotFound(err, ErrVendorNotFound)
}

func GetEmbyFirstVendor(userID string) (*model.EmbyVendor, error) {
	var vendor model.EmbyVendor
	err := db.Where("user_id = ?", userID).First(&vendor).Error
	return &vendor, HandleNotFound(err, ErrVendorNotFound)
}

func CreateOrSaveEmbyVendor(vendorInfo *model.EmbyVendor) (*model.EmbyVendor, error) {
	if vendorInfo.UserID == "" || vendorInfo.ServerID == "" {
		return nil, errors.New("user_id and server_id must not be empty")
	}
	return vendorInfo, Transactional(func(tx *gorm.DB) error {
		if errors.Is(tx.First(&model.EmbyVendor{
			UserID:   vendorInfo.UserID,
			ServerID: vendorInfo.ServerID,
		}).Error, gorm.ErrRecordNotFound) {
			return tx.Create(&vendorInfo).Error
		}
		result := tx.Omit("created_at").Save(&vendorInfo)
		return HandleUpdateResult(result, ErrVendorNotFound)
	})
}

func DeleteEmbyVendor(userID, serverID string) error {
	result := db.Where("user_id = ? AND server_id = ?", userID, serverID).Delete(&model.EmbyVendor{})
	return HandleUpdateResult(result, ErrVendorNotFound)
}
