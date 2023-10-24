package settings

import (
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type BoolSetting interface {
	Setting
	Set(bool) error
	Get() (bool, error)
	Default() bool
}

type Bool struct {
	name         string
	value        string
	defaultValue bool
	group        model.SettingGroup
}

func NewBool(name string, value bool, group model.SettingGroup) *Bool {
	b := &Bool{
		name:         name,
		group:        group,
		defaultValue: value,
	}
	if value {
		b.value = "1"
	} else {
		b.value = "0"
	}
	return b
}

func (b *Bool) Name() string {
	return b.name
}

func (b *Bool) Init(s string) {
	if b.value == s {
		return
	}
	b.value = s
}

func (b *Bool) Default() bool {
	return b.defaultValue
}

func (b *Bool) DefaultString() string {
	if b.defaultValue {
		return "1"
	} else {
		return "0"
	}
}

func (b *Bool) DefaultInterface() any {
	return b.Default()
}

func (b *Bool) SetString(value string) error {
	if b.value == value {
		return nil
	}
	b.value = value
	return db.UpdateSettingItemValue(b.name, value)
}

func (b *Bool) Set(value bool) error {
	if value {
		return b.SetString("1")
	} else {
		return b.SetString("0")
	}
}

func (b *Bool) Get() (bool, error) {
	return b.value == "1", nil
}

func (b *Bool) String() string {
	return b.value
}

func (b *Bool) Type() model.SettingType {
	return model.SettingTypeBool
}

func (b *Bool) Group() model.SettingGroup {
	return b.group
}

func (b *Bool) Interface() (any, error) {
	return b.Get()
}

func newBoolSetting(k string, v bool, g model.SettingGroup) BoolSetting {
	if Settings == nil {
		Settings = make(map[string]Setting)
	}
	if GroupSettings == nil {
		GroupSettings = make(map[model.SettingGroup][]Setting)
	}
	_, loaded := Settings[k]
	if loaded {
		log.Fatalf("setting %s already exists", k)
	}
	b := NewBool(k, v, g)
	Settings[k] = b
	GroupSettings[g] = append(GroupSettings[g], b)
	return b
}
