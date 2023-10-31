package settings

import (
	"fmt"

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
	value        string
}

func NewString(name string, value string, group model.SettingGroup) *String {
	s := &String{
		setting: setting{
			name:  name,
			group: group,
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
	s.value = v
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
	return s.Stringify(s.value)
}

func (s *String) SetRaw(value string) error {
	err := s.Init(value)
	if err != nil {
		return err
	}
	return db.UpdateSettingItemValue(s.Name(), s.Raw())
}

func (s *String) Set(value string) error {
	return s.SetRaw(value)
}

func (s *String) Get() string {
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
