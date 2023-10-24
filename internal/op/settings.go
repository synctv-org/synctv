package op

import (
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var (
	BoolSettings map[string]BoolSetting
)

type Setting interface {
	Name() string
	InitRaw(string)
	Raw() string
	Type() model.SettingType
	Group() model.SettingGroup
	Interface() (any, error)
}

func GetSettingByGroup(group model.SettingGroup) []Setting {
	return settingByGroup(group, ToSettings(BoolSettings)...)
}

func settingByGroup(group model.SettingGroup, settings ...Setting) []Setting {
	s := make([]Setting, 0, len(settings))
	for _, bs := range settings {
		if bs.Group() == group {
			s = append(s, bs)
		}
	}
	return s
}

func ToSettings[s Setting](settings ...map[string]s) []Setting {
	l := 0
	for _, v := range settings {
		l += len(v)
	}
	var ss []Setting = make([]Setting, 0, l)
	for _, v := range settings {
		for _, s := range v {
			ss = append(ss, s)
		}
	}
	return ss
}

type BoolSetting interface {
	Setting
	Set(bool) error
	Get() (bool, error)
}

type Bool struct {
	name  string
	value string
}

func NewBool(name, value string) *Bool {
	return &Bool{
		name:  name,
		value: value,
	}
}

func (b *Bool) Name() string {
	return b.name
}

func (b *Bool) InitRaw(s string) {
	if b.value == s {
		return
	}
	b.value = s
}

func (b *Bool) Set(value bool) error {
	if value {
		if b.value == "1" {
			return nil
		}
		b.value = "1"
	} else {
		if b.value == "0" {
			return nil
		}
		b.value = "0"
	}
	return db.UpdateSettingItemValue(b.name, b.value)
}

func (b *Bool) Get() (bool, error) {
	return b.value == "1", nil
}

func (b *Bool) Raw() string {
	return b.value
}

func (b *Bool) Type() model.SettingType {
	return model.SettingTypeBool
}

func (b *Bool) Group() model.SettingGroup {
	return model.SettingGroupRoom
}

func (b *Bool) Interface() (any, error) {
	return b.Get()
}

type Int64Setting interface {
	Set(int64) error
	Get() (int64, error)
	Raw() string
}

type Float64Setting interface {
	Set(float64) error
	Get() (float64, error)
	Raw() string
}

type StringSetting interface {
	Set(string) error
	Get() (string, error)
	Raw() string
}

func newRegBoolSetting(k, v string) BoolSetting {
	b := NewBool(k, v)
	if BoolSettings == nil {
		BoolSettings = make(map[string]BoolSetting)
	}
	BoolSettings[k] = b
	return b
}

func GetSettingType(name string) (model.SettingType, bool) {
	s, ok := BoolSettings[name]
	if !ok {
		return "", false
	}
	return s.Type(), true
}

var (
	DisableCreateRoom = newRegBoolSetting("disable_create_room", "0")
)
