package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm/clause"
)

func GetSettingItems() ([]*model.Setting, error) {
	var items []*model.Setting
	err := db.Find(&items).Error
	return items, err
}

func GetSettingItemByName(name string) (*model.Setting, error) {
	var item model.Setting
	err := db.Where("name = ?", name).First(&item).Error
	return &item, err
}

func SaveSettingItem(item *model.Setting) error {
	return db.Clauses(clause.OnConflict{
		UpdateAll: true,
	}).Save(item).Error
}

func DeleteSettingItem(item *model.Setting) error {
	return db.Delete(item).Error
}

func DeleteSettingItemByName(name string) error {
	return db.Where("name = ?", name).Delete(&model.Setting{}).Error
}

func GetSettingItemValue(name string) (string, error) {
	var value string
	err := db.Model(&model.Setting{}).Where("name = ?", name).Select("value").First(&value).Error
	return value, err
}

func FirstOrCreateSettingItemValue(s *model.Setting) error {
	return db.Where("name = ?", s.Name).Attrs(model.Setting{
		Value: s.Value,
		Type:  s.Type,
	}).FirstOrCreate(s).Error
}

func UpdateSettingItemValue(name, value string) error {
	return db.Model(&model.Setting{}).Where("name = ?", name).Update("value", value).Error
}
