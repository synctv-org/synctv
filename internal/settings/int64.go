package settings

import (
	"fmt"
	"strconv"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type Int64Setting interface {
	Setting
	Set(int64) error
	Get() int64
	Default() int64
	Parse(string) (int64, error)
	Stringify(int64) string
}

var _ Int64Setting = (*Int64)(nil)

type Int64 struct {
	setting
	defaultValue          int64
	value                 int64
	validator             func(int64) error
	beforeInit, beforeSet func(Int64Setting, int64) error
}

type Int64SettingOption func(*Int64)

func WithValidatorInt64(validator func(int64) error) Int64SettingOption {
	return func(s *Int64) {
		s.validator = validator
	}
}

func WithBeforeInitInt64(beforeInit func(Int64Setting, int64) error) Int64SettingOption {
	return func(s *Int64) {
		s.beforeInit = beforeInit
	}
}

func WithBeforeSetInt64(beforeSet func(Int64Setting, int64) error) Int64SettingOption {
	return func(s *Int64) {
		s.beforeSet = beforeSet
	}
}

func newInt64(name string, value int64, group model.SettingGroup, options ...Int64SettingOption) *Int64 {
	i := &Int64{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeInt64,
		},
		defaultValue: value,
		value:        value,
	}
	for _, option := range options {
		option(i)
	}
	return i
}

func (i *Int64) Parse(value string) (int64, error) {
	v, err := strconv.ParseInt(value, 10, 64)
	if err != nil {
		return 0, err
	}
	if i.validator != nil {
		return v, i.validator(v)
	}
	return v, nil
}

func (i *Int64) Stringify(value int64) string {
	return strconv.FormatInt(value, 10)
}

func (i *Int64) Init(value string) error {
	v, err := i.Parse(value)
	if err != nil {
		return err
	}

	if i.beforeInit != nil {
		err = i.beforeInit(i, v)
		if err != nil {
			return err
		}
	}

	i.set(v)
	return nil
}

func (i *Int64) Default() int64 {
	return i.defaultValue
}

func (i *Int64) DefaultString() string {
	return i.Stringify(i.defaultValue)
}

func (i *Int64) DefaultInterface() any {
	return i.Default()
}

func (i *Int64) String() string {
	return i.Stringify(i.Get())
}

func (i *Int64) SetString(value string) error {
	v, err := i.Parse(value)
	if err != nil {
		return err
	}

	if i.beforeSet != nil {
		err = i.beforeSet(i, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(i.name, i.Stringify(v))
	if err != nil {
		return err
	}

	i.set(v)
	return nil
}

func (i *Int64) set(value int64) {
	atomic.StoreInt64(&i.value, value)
}

func (i *Int64) Set(value int64) (err error) {
	if i.validator != nil {
		err = i.validator(value)
		if err != nil {
			return err
		}
	}

	if i.beforeSet != nil {
		err = i.beforeSet(i, value)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(i.name, i.Stringify(value))
	if err != nil {
		return err
	}

	i.set(value)
	return
}

func (i *Int64) Get() int64 {
	return atomic.LoadInt64(&i.value)
}

func (i *Int64) Interface() any {
	return i.Get()
}

func NewInt64Setting(k string, v int64, g model.SettingGroup, options ...Int64SettingOption) *Int64 {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	i := newInt64(k, v, g, options...)
	Settings[k] = i
	GroupSettings[g] = append(GroupSettings[g], i)
	return i
}
