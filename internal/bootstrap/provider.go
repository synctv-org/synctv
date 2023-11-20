package bootstrap

import (
	"context"
	"os"
	"path/filepath"

	"github.com/hashicorp/go-hclog"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/utils"
	"golang.org/x/oauth2"
)

func InitProvider(ctx context.Context) (err error) {
	logOur := log.StandardLogger().Writer()
	logLevle := hclog.Info
	if flags.Dev {
		logLevle = hclog.Debug
	}
	for _, op := range conf.Conf.OAuth2.Plugins {
		op.PluginFile, err = utils.OptFilePath(op.PluginFile)
		if err != nil {
			log.Fatalf("oauth2 plugin file path error: %v", err)
			return err
		}
		log.Infof("load oauth2 plugin: %s", op.PluginFile)
		err := os.MkdirAll(filepath.Dir(op.PluginFile), 0755)
		if err != nil {
			log.Fatalf("create plugin dir: %s failed: %s", filepath.Dir(op.PluginFile), err)
			return err
		}
		err = plugins.InitProviderPlugins(op.PluginFile, op.Arges, hclog.New(&hclog.LoggerOptions{
			Name:   op.PluginFile,
			Level:  logLevle,
			Output: logOur,
			Color:  hclog.ForceColor,
		}))
		if err != nil {
			log.Fatalf("load oauth2 plugin: %s failed: %s", op.PluginFile, err)
			return err
		}
	}
	for op, v := range conf.Conf.OAuth2.Providers {
		opt := provider.Oauth2Option{
			ClientID:     v.ClientID,
			ClientSecret: v.ClientSecret,
			RedirectURL:  v.RedirectURL,
		}
		if v.Endpoint != nil {
			opt.Endpoint = &oauth2.Endpoint{
				AuthURL:       v.Endpoint.AuthURL,
				DeviceAuthURL: v.Endpoint.DeviceAuthURL,
				TokenURL:      v.Endpoint.TokenURL,
			}
		}
		err := providers.InitProvider(op, opt)
		if err != nil {
			return err
		}
	}
	return nil
}
