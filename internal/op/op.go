package op

import (
	"time"

	"github.com/bluele/gcache"
	synccache "github.com/synctv-org/synctv/utils/syncCache"
)

func Init(size int, ttl time.Duration) error {
	roomTTL = ttl
	roomCache = synccache.NewSyncCache[uint, *Room](time.Minute*5, synccache.WithDeletedCallback[uint, *Room](func(v *Room) {
		v.close()
	}))
	userCache = gcache.New(size).
		LRU().
		Build()

	return nil
}
