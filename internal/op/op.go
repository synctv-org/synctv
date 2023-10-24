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

	si, err := db.GetSettingItems()
	if err != nil {
		panic(err)
	}
	for _, si2 := range si {
		switch si2.Type {
		case model.SettingTypeBool:
			b, ok := boolSettings[si2.Name]
			if ok {
				b.value = si2.Value
			}
		}
	}
	cleanReg()

	return nil
}
