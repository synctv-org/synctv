package setting

import (
	"fmt"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var (
	Settings      map[string]Setting
	GroupsSetting map[model.SettingGroup][]Setting
)

type Setting interface {
	Name() string
	InitRaw(string)
	Raw() string
	Type() model.SettingType
	Group() model.SettingGroup
	Interface() (any, error)
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

type Int64Setting interface {
	Set(int64) error
	Get() (int64, error)
	Raw() string
}

type Float64Setting interface {
	Set(float64) error
	Get() (float64, error)
	Raw() string
}

type StringSetting interface {
	Set(string) error
	Get() (string, error)
	Raw() string
}

func Init() error {
	return initSettings(ToSettings(Settings)...)
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
		b.InitRaw(s.Value)
	}
	return nil
}
