package model

import "time"

type SettingType string

const (
	SettingTypeBool    SettingType = "bool"
	SettingTypeInt64   SettingType = "int64"
	SettingTypeFloat64 SettingType = "float64"
	SettingTypeString  SettingType = "string"
)

type SettingGroup = string

const (
	SettingGroupRoom     SettingGroup = "room"
	SettingGroupUser     SettingGroup = "user"
	SettingGroupProxy    SettingGroup = "proxy"
	SettingGroupRtmp     SettingGroup = "rtmp"
	SettingGroupDatabase SettingGroup = "database"
	SettingGroupServer   SettingGroup = "server"
	SettingGroupOauth2   SettingGroup = "oauth2"
)

type Setting struct {
	Name      string `gorm:"primaryKey;type:varchar(256)"`
	UpdatedAt time.Time
	Value     string       `gorm:"not null;type:text"`
	Type      SettingType  `gorm:"not null;default:string"`
	Group     SettingGroup `gorm:"not null"`
}
