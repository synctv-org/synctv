package setting

import (
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type BoolSetting interface {
	Setting
	Set(bool) error
	Get() (bool, error)
}

type Bool struct {
	name  string
	value string
	group model.SettingGroup
}

func NewBool(name, value string, group model.SettingGroup) *Bool {
	return &Bool{
		name:  name,
		value: value,
		group: group,
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
	return b.group
}

func (b *Bool) Interface() (any, error) {
	return b.Get()
}

func newBoolSetting(k, v string, g model.SettingGroup) BoolSetting {
	if Settings == nil {
		Settings = make(map[string]Setting)
	}
	if GroupsSetting == nil {
		GroupsSetting = make(map[model.SettingGroup][]Setting)
	}
	_, loaded := Settings[k]
	if loaded {
		log.Fatalf("setting %s already exists", k)
	}
	b := NewBool(k, v, g)
	Settings[k] = b
	GroupsSetting[g] = append(GroupsSetting[g], b)
	return b
}
