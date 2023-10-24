package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm/clause"
)

func GetSettingItems() ([]*model.SettingItem, error) {
	var items []*model.SettingItem
	err := db.Find(&items).Error
	return items, err
}

func GetSettingItemByName(name string) (*model.SettingItem, error) {
	var item model.SettingItem
	err := db.Where("name = ?", name).First(&item).Error
	return &item, err
}

func SaveSettingItem(item *model.SettingItem) error {
	return db.Clauses(clause.OnConflict{
		UpdateAll: true,
	}).Save(item).Error
}

func DeleteSettingItem(item *model.SettingItem) error {
	return db.Delete(item).Error
}

func DeleteSettingItemByName(name string) error {
	return db.Where("name = ?", name).Delete(&model.SettingItem{}).Error
}

func GetSettingItemValue(name string) (string, error) {
	var value string
	err := db.Model(&model.SettingItem{}).Where("name = ?", name).Select("value").First(&value).Error
	return value, err
}

func SetSettingItemValue(name, value string) error {
	return db.Model(&model.SettingItem{}).Where("name = ?", name).Update("value", value).Error
}
