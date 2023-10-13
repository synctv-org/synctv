package bootstrap

import (
	"context"
	"sync"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/version"
	sysnotify "github.com/synctv-org/synctv/utils/sysNotify"
)

func InitCheckUpdate(ctx context.Context) error {
	v, err := version.NewVersionInfo()
	if err != nil {
		log.Fatalf("get version info error: %v", err)
	}
	go func() {
		t := time.NewTicker(time.Second)
		defer t.Stop()
		var (
			need   bool
			latest string
			url    string
			once   sync.Once
		)
		SysNotify.RegisterSysNotifyTask(0, sysnotify.NewSysNotifyTask(
			"check-update",
			sysnotify.NotifyTypeEXIT,
			func() error {
				if need {
					log.Infof("new version (%s) available: %s", latest, url)
					log.Infof("run 'synctv self-update' to auto update")
				}
				return nil
			},
		))
		for range t.C {
			l, err := v.CheckLatest(ctx)
			if err != nil {
				log.Errorf("check update error: %v", err)
				continue
			}
			latest = l
			b, err := v.NeedUpdate(ctx)
			if err != nil {
				log.Errorf("check update error: %v", err)
				continue
			}
			need = b
			if b {
				u, err := v.LatestBinaryURL(ctx)
				if err != nil {
					log.Errorf("check update error: %v", err)
					continue
				}
				url = u
			}
			once.Do(func() {
				if b {
					log.Infof("new version (%s) available: %s", latest, url)
					log.Infof("run 'synctv self-update' to auto update")
				}
				t.Reset(time.Hour * 6)
			})
		}
	}()
	return nil
}
