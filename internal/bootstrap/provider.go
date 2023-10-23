package bootstrap

import (
	"context"
	"os"
	"path/filepath"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/utils"
)

func InitProvider(ctx context.Context) error {
	for _, op := range conf.Conf.OAuth2.Plugins {
		utils.OptFilePath(&op.PluginFile)
		log.Infof("load oauth2 plugin: %s", op.PluginFile)
		err := os.MkdirAll(filepath.Dir(op.PluginFile), 0755)
		if err != nil {
			log.Errorf("create plugin dir: %s failed: %s", filepath.Dir(op.PluginFile), err)
			return err
		}
		err = provider.InitProviderPlugins(op.PluginFile, op.Arges...)
		if err != nil {
			log.Errorf("load oauth2 plugin: %s failed: %s", op.PluginFile, err)
			return err
		}
	}
	for op, v := range conf.Conf.OAuth2.Providers {
		err := provider.InitProvider(op, provider.Oauth2Option{
			ClientID:     v.ClientID,
			ClientSecret: v.ClientSecret,
			RedirectURL:  v.RedirectURL,
		})
		if err != nil {
			return err
		}
	}
	return nil
}
