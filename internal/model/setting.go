package model

type SettingType string

const (
	SettingTypeBool    SettingType = "bool"
	SettingTypeInt64   SettingType = "int64"
	SettingTypeFloat64 SettingType = "float64"
	SettingTypeString  SettingType = "string"
)

type SettingGroup string

const (
	SettingGroupRoom SettingGroup = "room"
	SettingGroupUser SettingGroup = "user"
)

type Setting struct {
	Name  string `gorm:"primaryKey"`
	Value string
	Type  SettingType  `gorm:"not null;default:string"`
	Group SettingGroup `gorm:"not null"`
}
