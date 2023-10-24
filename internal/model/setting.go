package model

type SettingItem struct {
	Name  string `gorm:"primaryKey"`
	Value string
}
