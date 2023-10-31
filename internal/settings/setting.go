package settings

import (
	"fmt"

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
	switch s.Type() {
	case model.SettingTypeBool:
		i, ok := s.(BoolSetting)
		if !ok {
			log.Fatalf("setting %s is not bool", s.Name())
		}
		v, ok := value.(bool)
		if !ok {
			return fmt.Errorf("setting %s, value %v is not bool", s.Name(), value)
		}
		i.Set(v)
	case model.SettingTypeInt64:
		i, ok := s.(Int64Setting)
		if !ok {
			log.Fatalf("setting %s is not int64", s.Name())
		}
		v, ok := value.(int64)
		if !ok {
			return fmt.Errorf("setting %s, value %v is not int64", s.Name(), value)
		}
		i.Set(v)
	case model.SettingTypeFloat64:
		i, ok := s.(Float64Setting)
		if !ok {
			log.Fatalf("setting %s is not float64", s.Name())
		}
		v, ok := value.(float64)
		if !ok {
			return fmt.Errorf("setting %s, value %v is not float64", s.Name(), value)
		}
		i.Set(v)
	case model.SettingTypeString:
		i, ok := s.(StringSetting)
		if !ok {
			log.Fatalf("setting %s is not string", s.Name())
		}
		v, ok := value.(string)
		if !ok {
			return fmt.Errorf("setting %s, value %v is not string", s.Name(), value)
		}
		i.Set(v)
	default:
		log.Fatalf("unknown setting type: %s", s.Type())
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
	name  string
	group model.SettingGroup
}

func (d *setting) Name() string {
	return d.name
}

func (d *setting) Type() model.SettingType {
	return model.SettingTypeString
}

func (d *setting) Group() model.SettingGroup {
	return d.group
}
