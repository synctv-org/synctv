package op

import (
	"time"

	"github.com/zijiren233/gencontainer/synccache"
)

func Init(size int) error {
	roomCache = synccache.NewSyncCache[string, *Room](time.Minute*5, synccache.WithDeletedCallback[string, *Room](func(v *Room) {
		v.close()
	}))
	userCache = synccache.NewSyncCache[string, *User](time.Minute * 5)

	return nil
}
