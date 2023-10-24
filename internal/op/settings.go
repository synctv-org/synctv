package op

import (
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var BoolSettings map[string]BoolSetting

type Setting interface {
	Name() string
	SetRaw(string)
	Raw() string
	Type() model.SettingType
}

func ToSettings[s Setting](settings map[string]s) []Setting {
	var ss []Setting
	for _, v := range settings {
		ss = append(ss, v)
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

func (b *Bool) SetRaw(s string) {
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
