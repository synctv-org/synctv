package settings

import (
	"fmt"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/model"
)

var (
	Settings      = make(map[string]Setting)
	GroupSettings = make(map[model.SettingGroup][]Setting)
)

type Setting interface {
	Name() string
	Type() model.SettingType
	Group() model.SettingGroup
	Init(string) error
	String() string
	SetString(string) error
	DefaultString() string
	DefaultInterface() any
	Interface() any
}

func SetValue(name string, value any) error {
	s, ok := Settings[name]
	if !ok {
		return fmt.Errorf("setting %s not found", name)
	}
	return s.SetString(json.Wrap(value).ToString())
}

type setting struct {
	name        string
	settingType model.SettingType
	group       model.SettingGroup
}

func (d *setting) Name() string {
	return d.name
}

func (d *setting) Type() model.SettingType {
	return d.settingType
}

func (d *setting) Group() model.SettingGroup {
	return d.group
}
