package model

type SettingType string

const (
	SettingTypeBool    SettingType = "bool"
	SettingTypeInt64   SettingType = "int64"
	SettingTypeFloat64 SettingType = "float64"
	SettingTypeString  SettingType = "string"
)

type Setting struct {
	Name  string `gorm:"primaryKey"`
	Value string
	Type  SettingType `gorm:"not null;default:string"`
}
