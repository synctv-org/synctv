package settings

import (
	"fmt"
	"sync"

	"github.com/synctv-org/synctv/internal/model"
)

type StringSetting interface {
	Setting
	Set(string) error
	Get() string
	Default() string
	Parse(string) (string, error)
	Stringify(string) string
}

var _ StringSetting = (*String)(nil)

type String struct {
	setting
	defaultValue string
	lock         sync.RWMutex
	value        string
	validator    func(string) error
	beforeSet    func(StringSetting, string) error
}

type StringSettingOption func(*String)

func WithValidatorString(validator func(string) error) StringSettingOption {
	return func(s *String) {
		s.validator = validator
	}
}

func WithBeforeSetString(beforeSet func(StringSetting, string) error) StringSettingOption {
	return func(s *String) {
		s.beforeSet = beforeSet
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
	s.set(v)
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
		err = s.beforeSet(s, v)
		if err != nil {
			return err
		}
	}

	s.set(v)
	return nil
}

func (s *String) set(value string) {
	s.lock.Lock()
	defer s.lock.Unlock()
	s.value = value
}

func (s *String) Set(value string) (err error) {
	if s.validator != nil {
		err = s.validator(value)
		if err != nil {
			return err
		}
	}

	if s.beforeSet != nil {
		err = s.beforeSet(s, value)
		if err != nil {
			return err
		}
	}

	s.set(value)
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

func NewStringSetting(k string, v string, g model.SettingGroup, options ...StringSettingOption) *String {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	s := newString(k, v, g, options...)
	Settings[k] = s
	GroupSettings[g] = append(GroupSettings[g], s)
	return s
}
