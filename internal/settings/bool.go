package settings

import (
	"fmt"
	"strconv"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type BoolSetting interface {
	Setting
	Set(bool) error
	Get() bool
	Default() bool
	Parse(string) (bool, error)
	Stringify(bool) string
}

var _ BoolSetting = (*Bool)(nil)

type Bool struct {
	setting
	defaultValue bool
	value        uint32
}

func NewBool(name string, value bool, group model.SettingGroup) *Bool {
	b := &Bool{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeBool,
		},
		defaultValue: value,
	}
	b.set(value)
	return b
}

func (b *Bool) set(value bool) {
	if value {
		atomic.StoreUint32(&b.value, 1)
	} else {
		atomic.StoreUint32(&b.value, 0)
	}
}

func (b *Bool) Get() bool {
	return atomic.LoadUint32(&b.value) == 1
}

func (b *Bool) Init(value string) error {
	v, err := b.Parse(value)
	if err != nil {
		return err
	}
	b.set(v)
	return nil
}

func (b *Bool) Parse(value string) (bool, error) {
	return strconv.ParseBool(value)
}

func (b *Bool) Stringify(value bool) string {
	return strconv.FormatBool(value)
}

func (b *Bool) Default() bool {
	return b.defaultValue
}

func (b *Bool) DefaultRaw() string {
	return b.Stringify(b.defaultValue)
}

func (b *Bool) Raw() string {
	return b.Stringify(b.Get())
}

func (b *Bool) DefaultInterface() any {
	return b.Default()
}

func (b *Bool) SetRaw(value string) error {
	err := b.Init(value)
	if err != nil {
		return err
	}
	return db.UpdateSettingItemValue(b.Name(), b.Raw())
}

func (b *Bool) Set(value bool) error {
	err := db.UpdateSettingItemValue(b.Name(), b.Stringify(value))
	if err != nil {
		return err
	}
	b.set(value)
	return nil
}

func (b *Bool) Interface() any {
	return b.Get()
}

func newBoolSetting(k string, v bool, g model.SettingGroup) BoolSetting {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	b := NewBool(k, v, g)
	Settings[k] = b
	GroupSettings[g] = append(GroupSettings[g], b)
	return b
}
