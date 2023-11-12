package settings

import (
	"fmt"

	json "github.com/json-iterator/go"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
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
	Raw() string
	SetRaw(string) error
	DefaultRaw() string
	DefaultInterface() any
	Interface() any
}

func SetValue(name string, value any) error {
	s, ok := Settings[name]
	if !ok {
		return fmt.Errorf("setting %s not found", name)
	}
	return SetSettingValue(s, value)
}

func SetSettingValue(s Setting, value any) error {
	switch s := s.(type) {
	case BoolSetting:
		return s.Set(json.Wrap(value).ToBool())
	case Int64Setting:
		return s.Set(json.Wrap(value).ToInt64())
	case Float64Setting:
		return s.Set(json.Wrap(value).ToFloat64())
	case StringSetting:
		return s.Set(json.Wrap(value).ToString())
	default:
		log.Fatalf("unknown setting %s type: %s", s.Name(), s.Type())
	}
	return nil
}

func ToSettings[s Setting](settings map[string]s) []Setting {
	var ss []Setting = make([]Setting, 0, len(settings))
	for _, v := range settings {
		ss = append(ss, v)
	}
	return ss
}

func Init() error {
	return initAndFixSettings(ToSettings(Settings)...)
}

func initSettings(i ...Setting) error {
	for _, b := range i {
		s := &model.Setting{
			Name:  b.Name(),
			Value: b.Raw(),
			Type:  b.Type(),
			Group: b.Group(),
		}
		err := db.FirstOrCreateSettingItemValue(s)
		if err != nil {
			return err
		}
		err = b.Init(s.Value)
		if err != nil {
			return err
		}
	}
	return nil
}

func initAndFixSettings(i ...Setting) error {
	for _, b := range i {
		s := &model.Setting{
			Name:  b.Name(),
			Value: b.Raw(),
			Type:  b.Type(),
			Group: b.Group(),
		}
		err := db.FirstOrCreateSettingItemValue(s)
		if err != nil {
			return err
		}
		err = b.Init(s.Value)
		if err != nil {
			// auto fix
			err = b.SetRaw(b.DefaultRaw())
			if err != nil {
				return err
			}
		}
	}
	return nil
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
