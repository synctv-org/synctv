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
	settingsCache, err := db.GetSettingItemsToMap()
	if err != nil {
		return err
	}
	for _, b := range s {
		if s, ok := settingsCache[b.Name()]; ok {
			err = b.Init(s.Value)
			if err != nil {
				return err
			}
		} else {
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
	}
	return nil
}

func settingEqual(s *model.Setting, b settings.Setting) bool {
	return s.Type == b.Type() && s.Group == b.Group() && s.Name == b.Name()
}

func initAndFixSettings(s map[string]settings.Setting) error {
	settingsCache, err := db.GetSettingItemsToMap()
	if err != nil {
		return err
	}
	var (
		setting *model.Setting
	)
	for _, b := range s {
		if sc, ok := settingsCache[b.Name()]; ok && settingEqual(sc, b) {
			setting = sc
		} else {
			setting = &model.Setting{
				Name:  b.Name(),
				Value: b.DefaultString(),
				Type:  b.Type(),
				Group: b.Group(),
			}
			err := db.FirstOrCreateSettingItemValue(setting)
			if err != nil {
				return err
			}
		}
		err = b.Init(setting.Value)
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
