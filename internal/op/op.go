package op

import (
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/zijiren233/gencontainer/synccache"
)

func Init(_ int) error {
	roomCache = synccache.NewSyncCache(
		time.Minute*5,
		synccache.WithDeletedCallback[string](func(v *Room) {
			log.WithFields(log.Fields{
				"rid": v.ID,
				"rn":  v.Name,
			}).Debugf("room ttl expired, closing")
			v.close()
		}),
	)
	userCache = synccache.NewSyncCache[string, *User](time.Minute * 5)

	return nil
}
