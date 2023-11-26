package bootstrap

import (
	"context"
	"fmt"
	"os"
	"path/filepath"

	"github.com/hashicorp/go-hclog"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/internal/settings"
	serverAuth "github.com/synctv-org/synctv/server/oauth2"
	"github.com/synctv-org/synctv/utils"
)

var ProviderGroupSettings = make(map[model.SettingGroup][]settings.Setting)

func InitProvider(ctx context.Context) (err error) {
	logOur := log.StandardLogger().Writer()
	logLevle := hclog.Info
	if flags.Dev {
		logLevle = hclog.Debug
	}
	for _, op := range conf.Conf.Oauth2Plugins {
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
		err = plugins.InitProviderPlugins(op.PluginFile, op.Args, hclog.New(&hclog.LoggerOptions{
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

	for op, pi := range providers.AllProvider() {
		op, pi := op, pi
		group := model.SettingGroup(fmt.Sprintf("%s_%s", model.SettingGroupOauth2, op))

		e := settings.NewBoolSetting(fmt.Sprintf("%s_enabled", group), false, group, settings.WithBeforeInitBool(func(bs settings.BoolSetting, b bool) error {
			defer serverAuth.Oauth2EnabledCache.Refresh()
			if b {
				return providers.EnableProvider(op)
			} else {
				providers.DisableProvider(op)
				return nil
			}
		}), settings.WithBeforeSetBool(func(bs settings.BoolSetting, b bool) error {
			defer serverAuth.Oauth2EnabledCache.Refresh()
			if b {
				return providers.EnableProvider(op)
			} else {
				providers.DisableProvider(op)
				return nil
			}
		}))
		ProviderGroupSettings[group] = []settings.Setting{e}

		opt := provider.Oauth2Option{}

		cid := settings.NewStringSetting(fmt.Sprintf("%s_client_id", group), opt.ClientID, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.ClientID = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.ClientID = s
			pi.Init(opt)
			return nil
		}))
		ProviderGroupSettings[group] = append(ProviderGroupSettings[group], cid)

		cs := settings.NewStringSetting(fmt.Sprintf("%s_client_secret", group), opt.ClientSecret, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.ClientSecret = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.ClientSecret = s
			pi.Init(opt)
			return nil
		}))
		ProviderGroupSettings[group] = append(ProviderGroupSettings[group], cs)

		ru := settings.NewStringSetting(fmt.Sprintf("%s_redirect_url", group), opt.RedirectURL, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.RedirectURL = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.RedirectURL = s
			pi.Init(opt)
			return nil
		}))
		ProviderGroupSettings[group] = append(ProviderGroupSettings[group], ru)
	}
	return nil
}
