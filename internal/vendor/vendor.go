package vendor

import (
	klog "github.com/go-kratos/kratos/v2/log"
	"github.com/go-kratos/kratos/v2/selector"
	"github.com/go-kratos/kratos/v2/selector/wrr"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
)

func Init(conf *conf.VendorConfig) error {
	klog.SetLogger(klog.NewStdLogger(log.StandardLogger().Writer()))
	selector.SetGlobalSelector(wrr.NewBuilder())
	if err := InitBilibili(&conf.Bilibili); err != nil {
		return err
	}
	return nil
}
