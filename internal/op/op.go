package op

import (
	"github.com/bluele/gcache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

func Init(size int) error {
	userCache = gcache.New(size).
		LRU().
		Build()

	err := initSettings(ToSettings(BoolSettings)...)
	if err != nil {
		return err
	}

	return nil
}

func initSettings(i ...Setting) error {
	for _, b := range i {
		s := &model.Setting{
			Name:  b.Name(),
			Value: b.Raw(),
			Type:  model.SettingTypeBool,
		}
		err := db.FirstOrCreateSettingItemValue(s)
		if err != nil {
			return err
		}
		b.SetRaw(s.Value)
	}
	return nil
}
