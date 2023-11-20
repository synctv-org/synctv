package bootstrap

import (
	"context"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
)

func InitSetting(ctx context.Context) error {
	return initAndFixSettings(settings.Settings)
}

func initSettings(s map[string]settings.Setting) error {
	for _, b := range s {
		s := &model.Setting{
			Name:  b.Name(),
			Value: b.DefaultString(),
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

func initAndFixSettings(s map[string]settings.Setting) error {
	for _, b := range s {
		s := &model.Setting{
			Name:  b.Name(),
			Value: b.DefaultString(),
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
			err = b.SetString(b.DefaultString())
			if err != nil {
				return err
			}
		}
	}
	return nil
}
