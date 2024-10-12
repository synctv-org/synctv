package op

import (
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/zijiren233/gencontainer/synccache"
)

func Init(size int) error {
	roomCache = synccache.NewSyncCache[string, *Room](time.Minute*5, synccache.WithDeletedCallback[string, *Room](func(v *Room) {
		log.WithFields(log.Fields{
			"rid": v.ID,
			"rn":  v.Name,
		}).Debugf("room ttl expired, closing")
		v.close()
	}))
	userCache = synccache.NewSyncCache[string, *User](time.Minute * 5)

	return nil
}
