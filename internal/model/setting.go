package model

type SettingType string

const (
	SettingTypeBool    SettingType = "bool"
	SettingTypeInt64   SettingType = "int64"
	SettingTypeFloat64 SettingType = "float64"
	SettingTypeString  SettingType = "string"
)

type SettingGroup string

func (s SettingGroup) String() string {
	return string(s)
}

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
	Name  string `gorm:"primaryKey"`
	Value string
	Type  SettingType  `gorm:"not null;default:string"`
	Group SettingGroup `gorm:"not null"`
}
