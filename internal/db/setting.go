package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm/clause"
)

func GetSettingItems() ([]*model.Setting, error) {
	var items []*model.Setting
	return items, db.Find(&items).Error
}

func GetSettingItemsToMap() (map[string]*model.Setting, error) {
	items, err := GetSettingItems()
	if err != nil {
		return nil, err
	}
	m := make(map[string]*model.Setting, len(items))
	for _, item := range items {
		m[item.Name] = item
	}
	return m, nil
}

func GetSettingItemByName(name string) (*model.Setting, error) {
	var item model.Setting
	err := db.Where("name = ?", name).First(&item).Error
	if err != nil {
		return nil, err
	}
	return &item, nil
}

func SaveSettingItem(item *model.Setting) error {
	result := db.Clauses(clause.OnConflict{
		UpdateAll: true,
	}).Create(item)
	return HandleUpdateResult(result, "setting")
}

func DeleteSettingItem(item *model.Setting) error {
	result := db.Where("name = ?", item.Name).Delete(&model.Setting{})
	return HandleUpdateResult(result, "setting")
}

func DeleteSettingItemByName(name string) error {
	result := db.Where("name = ?", name).Delete(&model.Setting{})
	return HandleUpdateResult(result, "setting")
}

func GetSettingItemValue(name string) (string, error) {
	var value string
	err := db.Model(&model.Setting{}).Where("name = ?", name).Select("value").Take(&value).Error
	if err != nil {
		return "", err
	}
	return value, nil
}

func FirstOrCreateSettingItemValue(s *model.Setting) error {
	return db.Where("name = ?", s.Name).FirstOrCreate(s, model.Setting{
		Value: s.Value,
		Type:  s.Type,
		Group: s.Group,
	}).Error
}

func UpdateSettingItemValue(name, value string) error {
	result := db.Model(&model.Setting{}).Where("name = ?", name).Update("value", value)
	return HandleUpdateResult(result, "setting")
}
