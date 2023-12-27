package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func GetBilibiliVendor(userID string) (*model.BilibiliVendor, error) {
	var vendor model.BilibiliVendor
	err := db.Where("user_id = ?", userID).First(&vendor).Error
	return &vendor, HandleNotFound(err, "vendor")
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
		} else {
			return tx.Omit("created_at").Save(&vendorInfo).Error
		}
	})
}

func DeleteBilibiliVendor(userID string) error {
	return db.Where("user_id = ?", userID).Delete(&model.BilibiliVendor{}).Error
}

func GetAlistVendor(userID string) (*model.AlistVendor, error) {
	var vendor model.AlistVendor
	err := db.Where("user_id = ?", userID).First(&vendor).Error
	return &vendor, HandleNotFound(err, "vendor")
}

func CreateOrSaveAlistVendor(vendorInfo *model.AlistVendor) (*model.AlistVendor, error) {
	if vendorInfo.UserID == "" {
		return nil, errors.New("user_id must not be empty")
	}
	return vendorInfo, Transactional(func(tx *gorm.DB) error {
		if errors.Is(tx.First(&model.AlistVendor{
			UserID: vendorInfo.UserID,
		}).Error, gorm.ErrRecordNotFound) {
			return tx.Create(&vendorInfo).Error
		} else {
			return tx.Omit("created_at").Save(&vendorInfo).Error
		}
	})
}

func DeleteAlistVendor(userID string) error {
	return db.Where("user_id = ?", userID).Delete(&model.AlistVendor{}).Error
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
	return &vendor, HandleNotFound(err, "vendor")
}

func GetEmbyFirstVendor(userID string) (*model.EmbyVendor, error) {
	var vendor model.EmbyVendor
	err := db.Where("user_id = ?", userID).First(&vendor).Error
	return &vendor, HandleNotFound(err, "vendor")
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
		} else {
			return tx.Omit("created_at").Save(&vendorInfo).Error
		}
	})
}

func DeleteEmbyVendor(userID, serverID string) error {
	return db.Where("user_id = ? AND server_id = ?", userID, serverID).Delete(&model.EmbyVendor{}).Error
}
