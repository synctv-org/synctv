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
	defaultValue          bool
	value                 uint32
	beforeInit, beforeSet func(BoolSetting, bool) (bool, error)
}

type BoolSettingOption func(*Bool)

func WithBeforeInitBool(beforeInit func(BoolSetting, bool) (bool, error)) BoolSettingOption {
	return func(s *Bool) {
		s.beforeInit = beforeInit
	}
}

func WithBeforeSetBool(beforeSet func(BoolSetting, bool) (bool, error)) BoolSettingOption {
	return func(s *Bool) {
		s.beforeSet = beforeSet
	}
}

func newBool(name string, value bool, group model.SettingGroup, options ...BoolSettingOption) *Bool {
	b := &Bool{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeBool,
		},
		defaultValue: value,
	}
	for _, option := range options {
		option(b)
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

	if b.beforeInit != nil {
		v, err = b.beforeInit(b, v)
		if err != nil {
			return err
		}
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

func (b *Bool) DefaultString() string {
	return b.Stringify(b.defaultValue)
}

func (b *Bool) String() string {
	return b.Stringify(b.Get())
}

func (b *Bool) DefaultInterface() any {
	return b.Default()
}

func (b *Bool) SetString(value string) error {
	v, err := b.Parse(value)
	if err != nil {
		return err
	}

	if b.beforeSet != nil {
		v, err = b.beforeSet(b, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(b.name, b.Stringify(v))
	if err != nil {
		return err
	}

	b.set(v)
	return nil
}

func (b *Bool) Set(v bool) (err error) {
	if b.beforeSet != nil {
		v, err = b.beforeSet(b, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(b.name, b.Stringify(v))
	if err != nil {
		return err
	}

	b.set(v)
	return
}

func (b *Bool) Interface() any {
	return b.Get()
}

func NewBoolSetting(k string, v bool, g model.SettingGroup, options ...BoolSettingOption) BoolSetting {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	b := newBool(k, v, g, options...)
	Settings[k] = b
	GroupSettings[g] = append(GroupSettings[g], b)
	return b
}
