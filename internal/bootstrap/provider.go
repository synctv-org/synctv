package bootstrap

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"time"

	"github.com/hashicorp/go-hclog"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/gencontainer/vec"
)

var ProviderGroupSettings = make(map[model.SettingGroup]*ProviderGroupSetting)

type ProviderGroupSetting struct {
	Enabled           settings.BoolSetting
	ClientID          settings.StringSetting
	ClientSecret      settings.StringSetting
	RedirectURL       settings.StringSetting
	DisableUserSignup settings.BoolSetting
	SignupNeedReview  settings.BoolSetting
}

var (
	Oauth2EnabledCache = refreshcache.NewRefreshCache[[]provider.OAuth2Provider](func() ([]provider.OAuth2Provider, error) {
		a := vec.New[provider.OAuth2Provider](vec.WithCmpEqual[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 == v2
		}), vec.WithCmpLess[provider.OAuth2Provider](func(v1, v2 provider.OAuth2Provider) bool {
			return v1 < v2
		}))
		providers.EnabledProvider().Range(func(key provider.OAuth2Provider, value provider.ProviderInterface) bool {
			a.Push(key)
			return true
		})
		return a.SortStable().Slice(), nil
	}, time.Hour)
)

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
		groupSettings := &ProviderGroupSetting{}
		ProviderGroupSettings[group] = groupSettings

		groupSettings.Enabled = settings.NewBoolSetting(fmt.Sprintf("%s_enabled", group), false, group, settings.WithBeforeInitBool(func(bs settings.BoolSetting, b bool) error {
			defer Oauth2EnabledCache.Refresh()
			if b {
				return providers.EnableProvider(op)
			} else {
				providers.DisableProvider(op)
				return nil
			}
		}), settings.WithBeforeSetBool(func(bs settings.BoolSetting, b bool) error {
			defer Oauth2EnabledCache.Refresh()
			if b {
				return providers.EnableProvider(op)
			} else {
				providers.DisableProvider(op)
				return nil
			}
		}))

		opt := provider.Oauth2Option{}

		groupSettings.ClientID = settings.NewStringSetting(fmt.Sprintf("%s_client_id", group), opt.ClientID, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.ClientID = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.ClientID = s
			pi.Init(opt)
			return nil
		}))

		groupSettings.ClientSecret = settings.NewStringSetting(fmt.Sprintf("%s_client_secret", group), opt.ClientSecret, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.ClientSecret = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.ClientSecret = s
			pi.Init(opt)
			return nil
		}))

		groupSettings.RedirectURL = settings.NewStringSetting(fmt.Sprintf("%s_redirect_url", group), opt.RedirectURL, group, settings.WithBeforeInitString(func(ss settings.StringSetting, s string) error {
			opt.RedirectURL = s
			pi.Init(opt)
			return nil
		}), settings.WithBeforeSetString(func(ss settings.StringSetting, s string) error {
			opt.RedirectURL = s
			pi.Init(opt)
			return nil
		}))

		groupSettings.DisableUserSignup = settings.NewBoolSetting(fmt.Sprintf("%s_disable_user_signup", group), false, group)

		groupSettings.SignupNeedReview = settings.NewBoolSetting(fmt.Sprintf("%s_signup_need_review", group), false, group)
	}
	return nil
}
