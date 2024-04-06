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
	SetBeforeInit(func(Int64Setting, int64) (int64, error))
	SetBeforeSet(func(Int64Setting, int64) (int64, error))
}

var _ Int64Setting = (*Int64)(nil)

type Int64 struct {
	setting
	defaultValue          int64
	value                 int64
	validator             func(int64) error
	beforeInit, beforeSet func(Int64Setting, int64) (int64, error)
	afterInit, afterSet   func(Int64Setting, int64)
}

type Int64SettingOption func(*Int64)

func WithInitPriorityInt64(priority int) Int64SettingOption {
	return func(s *Int64) {
		s.SetInitPriority(priority)
	}
}

func WithValidatorInt64(validator func(int64) error) Int64SettingOption {
	return func(s *Int64) {
		s.validator = validator
	}
}

func WithBeforeInitInt64(beforeInit func(Int64Setting, int64) (int64, error)) Int64SettingOption {
	return func(s *Int64) {
		s.SetBeforeInit(beforeInit)
	}
}

func WithBeforeSetInt64(beforeSet func(Int64Setting, int64) (int64, error)) Int64SettingOption {
	return func(s *Int64) {
		s.SetBeforeSet(beforeSet)
	}
}

func WithAfterInitInt64(afterInit func(Int64Setting, int64)) Int64SettingOption {
	return func(s *Int64) {
		s.SetAfterInit(afterInit)
	}
}

func WithAfterSetInt64(afterSet func(Int64Setting, int64)) Int64SettingOption {
	return func(s *Int64) {
		s.SetAfterSet(afterSet)
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

func (i *Int64) SetInitPriority(priority int) {
	i.initPriority = priority
}

func (i *Int64) SetBeforeInit(beforeInit func(Int64Setting, int64) (int64, error)) {
	i.beforeInit = beforeInit
}

func (i *Int64) SetBeforeSet(beforeSet func(Int64Setting, int64) (int64, error)) {
	i.beforeSet = beforeSet
}

func (i *Int64) SetAfterInit(afterInit func(Int64Setting, int64)) {
	i.afterInit = afterInit
}

func (i *Int64) SetAfterSet(afterSet func(Int64Setting, int64)) {
	i.afterSet = afterSet
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
		v, err = i.beforeInit(i, v)
		if err != nil {
			return err
		}
	}

	i.set(v)

	if i.afterInit != nil {
		i.afterInit(i, v)
	}

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
		v, err = i.beforeSet(i, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(i.name, i.Stringify(v))
	if err != nil {
		return err
	}

	i.set(v)

	if i.afterSet != nil {
		i.afterSet(i, v)
	}

	return nil
}

func (i *Int64) set(value int64) {
	atomic.StoreInt64(&i.value, value)
}

func (i *Int64) Set(v int64) (err error) {
	if i.validator != nil {
		err = i.validator(v)
		if err != nil {
			return err
		}
	}

	if i.beforeSet != nil {
		v, err = i.beforeSet(i, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(i.name, i.Stringify(v))
	if err != nil {
		return err
	}

	i.set(v)

	if i.afterSet != nil {
		i.afterSet(i, v)
	}

	return
}

func (i *Int64) Get() int64 {
	return atomic.LoadInt64(&i.value)
}

func (i *Int64) Interface() any {
	return i.Get()
}

func NewInt64Setting(k string, v int64, g model.SettingGroup, options ...Int64SettingOption) Int64Setting {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	return CoverInt64Setting(k, v, g, options...)
}

func CoverInt64Setting(k string, v int64, g model.SettingGroup, options ...Int64SettingOption) Int64Setting {
	i := newInt64(k, v, g, options...)
	Settings[k] = i
	if GroupSettings[g] == nil {
		GroupSettings[g] = make(map[string]Setting)
	}
	GroupSettings[g][k] = i
	return i
}

func LoadInt64Setting(k string) (Int64Setting, bool) {
	s, ok := Settings[k]
	if !ok {
		return nil, false
	}
	i, ok := s.(Int64Setting)
	return i, ok
}

func LoadOrNewInt64Setting(k string, v int64, g model.SettingGroup, options ...Int64SettingOption) Int64Setting {
	s, ok := LoadInt64Setting(k)
	if ok {
		return s
	}
	return CoverInt64Setting(k, v, g, options...)
}
