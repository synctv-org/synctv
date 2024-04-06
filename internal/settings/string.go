package settings

import (
	"fmt"
	"sync"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type StringSetting interface {
	Setting
	Set(string) error
	Get() string
	Default() string
	Parse(string) (string, error)
	Stringify(string) string
	SetBeforeInit(func(StringSetting, string) (string, error))
	SetBeforeSet(func(StringSetting, string) (string, error))
}

var _ StringSetting = (*String)(nil)

type String struct {
	setting
	defaultValue          string
	lock                  sync.RWMutex
	value                 string
	validator             func(string) error
	beforeInit, beforeSet func(StringSetting, string) (string, error)
	afterInit, afterSet   func(StringSetting, string)
}

type StringSettingOption func(*String)

func WithInitPriorityString(priority int) StringSettingOption {
	return func(s *String) {
		s.SetInitPriority(priority)
	}
}

func WithValidatorString(validator func(string) error) StringSettingOption {
	return func(s *String) {
		s.validator = validator
	}
}

func WithBeforeInitString(beforeInit func(StringSetting, string) (string, error)) StringSettingOption {
	return func(s *String) {
		s.SetBeforeInit(beforeInit)
	}
}

func WithBeforeSetString(beforeSet func(StringSetting, string) (string, error)) StringSettingOption {
	return func(s *String) {
		s.SetBeforeSet(beforeSet)
	}
}

func WithAfterInitString(afterInit func(StringSetting, string)) StringSettingOption {
	return func(s *String) {
		s.SetAfterInit(afterInit)
	}
}

func WithAfterSetString(afterSet func(StringSetting, string)) StringSettingOption {
	return func(s *String) {
		s.SetAfterSet(afterSet)
	}
}

func newString(name string, value string, group model.SettingGroup, options ...StringSettingOption) *String {
	s := &String{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeString,
		},
		defaultValue: value,
		value:        value,
	}
	for _, option := range options {
		option(s)
	}
	return s
}

func (s *String) SetInitPriority(priority int) {
	s.initPriority = priority
}

func (s *String) SetBeforeInit(beforeInit func(StringSetting, string) (string, error)) {
	s.beforeInit = beforeInit
}

func (s *String) SetBeforeSet(beforeSet func(StringSetting, string) (string, error)) {
	s.beforeSet = beforeSet
}

func (s *String) SetAfterInit(afterInit func(StringSetting, string)) {
	s.afterInit = afterInit
}

func (s *String) SetAfterSet(afterSet func(StringSetting, string)) {
	s.afterSet = afterSet
}

func (s *String) Parse(value string) (string, error) {
	if s.validator != nil {
		return value, s.validator(value)
	}
	return value, nil
}

func (s *String) Stringify(value string) string {
	return value
}

func (s *String) Init(value string) error {
	v, err := s.Parse(value)
	if err != nil {
		return err
	}

	if s.beforeInit != nil {
		v, err = s.beforeInit(s, v)
		if err != nil {
			return err
		}
	}

	s.set(v)

	if s.afterInit != nil {
		s.afterInit(s, v)
	}

	return nil
}

func (s *String) Default() string {
	return s.defaultValue
}

func (s *String) DefaultString() string {
	return s.Stringify(s.defaultValue)
}

func (s *String) DefaultInterface() any {
	return s.Default()
}

func (s *String) String() string {
	return s.Stringify(s.Get())
}

func (s *String) SetString(value string) error {
	v, err := s.Parse(value)
	if err != nil {
		return err
	}

	if s.beforeSet != nil {
		v, err = s.beforeSet(s, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(s.name, s.Stringify(v))
	if err != nil {
		return err
	}

	s.set(v)

	if s.afterSet != nil {
		s.afterSet(s, v)
	}

	return nil
}

func (s *String) set(value string) {
	s.lock.Lock()
	defer s.lock.Unlock()
	s.value = value
}

func (s *String) Set(v string) (err error) {
	if s.validator != nil {
		err = s.validator(v)
		if err != nil {
			return err
		}
	}

	if s.beforeSet != nil {
		v, err = s.beforeSet(s, v)
		if err != nil {
			return err
		}
	}

	err = db.UpdateSettingItemValue(s.name, s.Stringify(v))
	if err != nil {
		return err
	}

	s.set(v)

	if s.afterSet != nil {
		s.afterSet(s, v)
	}

	return
}

func (s *String) Get() string {
	s.lock.RLock()
	defer s.lock.RUnlock()
	return s.value
}

func (s *String) Interface() any {
	return s.Get()
}

func NewStringSetting(k string, v string, g model.SettingGroup, options ...StringSettingOption) StringSetting {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	return CoverStringSetting(k, v, g, options...)
}

func CoverStringSetting(k string, v string, g model.SettingGroup, options ...StringSettingOption) StringSetting {
	s := newString(k, v, g, options...)
	Settings[k] = s
	if GroupSettings[g] == nil {
		GroupSettings[g] = make(map[string]Setting)
	}
	GroupSettings[g][k] = s
	return s
}

func LoadStringSetting(k string) (StringSetting, bool) {
	s, ok := Settings[k]
	if !ok {
		return nil, false
	}
	ss, ok := s.(StringSetting)
	return ss, ok
}

func LoadOrNewStringSetting(k string, v string, g model.SettingGroup, options ...StringSettingOption) StringSetting {
	s, ok := LoadStringSetting(k)
	if ok {
		return s
	}
	return NewStringSetting(k, v, g, options...)
}
