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
}

var _ StringSetting = (*String)(nil)

type String struct {
	setting
	defaultValue string
	lock         sync.RWMutex
	value        string
}

func NewString(name string, value string, group model.SettingGroup) *String {
	s := &String{
		setting: setting{
			name:        name,
			group:       group,
			settingType: model.SettingTypeString,
		},
		defaultValue: value,
		value:        value,
	}
	return s
}

func (s *String) Parse(value string) (string, error) {
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

func (s *String) DefaultRaw() string {
	return s.defaultValue
}

func (s *String) DefaultInterface() any {
	return s.Default()
}

func (s *String) Raw() string {
	return s.Stringify(s.Get())
}

func (s *String) SetRaw(value string) error {
	err := s.Init(value)
	if err != nil {
		return err
	}
	return db.UpdateSettingItemValue(s.Name(), s.Raw())
}

func (s *String) set(value string) {
	s.lock.Lock()
	defer s.lock.Unlock()
	s.value = value
}

func (s *String) Set(value string) error {
	err := db.UpdateSettingItemValue(s.Name(), s.Stringify(value))
	if err != nil {
		return err
	}
	s.set(value)
	return nil
}

func (s *String) Get() string {
	s.lock.RLock()
	defer s.lock.RUnlock()
	return s.value
}

func (s *String) Interface() any {
	return s.Get()
}

func newStringSetting(k string, v string, g model.SettingGroup) *String {
	_, loaded := Settings[k]
	if loaded {
		panic(fmt.Sprintf("setting %s already exists", k))
	}
	s := NewString(k, v, g)
	Settings[k] = s
	GroupSettings[g] = append(GroupSettings[g], s)
	return s
}
