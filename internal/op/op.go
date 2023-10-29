package op

import (
	"time"

	"github.com/bluele/gcache"
	synccache "github.com/synctv-org/synctv/utils/syncCache"
)

func Init(size int, ttl time.Duration) error {
	roomTTL = ttl
	roomCache = synccache.NewSyncCache[string, *Room](time.Minute*5, synccache.WithDeletedCallback[string, *Room](func(v *Room) {
		v.close()
	}))
	userCache = gcache.New(size).
		LRU().
		Build()

	return nil
}
