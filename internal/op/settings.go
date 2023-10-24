package op

import (
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var boolSettings map[string]*Bool

type Setting interface {
	Name() string
	Raw() string
	Type() model.SettingType
}

type BoolSetting interface {
	Setting
	Set(value bool) error
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

func (b *Bool) Set(value bool) error {
	if value {
		b.value = "1"
	} else {
		b.value = "0"
	}
	return db.SetSettingItemValue(b.name, b.value)
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
	Set(value int64) error
	Get() (int64, error)
	Raw() string
}

type Float64Setting interface {
	Set(value float64) error
	Get() (float64, error)
	Raw() string
}

type StringSetting interface {
	Set(value string) error
	Get() (string, error)
	Raw() string
}

func cleanReg() {
	boolSettings = nil
}

func newRegBoolSetting(k, v string) BoolSetting {
	b := NewBool(k, v)
	if boolSettings == nil {
		boolSettings = make(map[string]*Bool)
	}
	boolSettings[k] = b
	return b
}

var (
	DisableCreateRoom = newRegBoolSetting("disable_create_room", "0")
)
