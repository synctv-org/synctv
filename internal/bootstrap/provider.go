package bootstrap

import (
	"context"
	"fmt"
	"os"
	"path/filepath"
	"slices"
	"strings"

	"github.com/hashicorp/go-hclog"
	"github.com/maruel/natural"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/aggregations"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"github.com/synctv-org/synctv/internal/provider/providers"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/refreshcache"
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

var Oauth2EnabledCache = refreshcache.NewRefreshCache[[]provider.OAuth2Provider](func(context.Context, ...any) ([]provider.OAuth2Provider, error) {
	ps := providers.EnabledProvider()
	r := make([]provider.OAuth2Provider, 0, ps.Len())
	providers.EnabledProvider().Range(func(p provider.OAuth2Provider, value struct{}) bool {
		r = append(r, p)
		return true
	})
	slices.SortStableFunc(r, func(a, b provider.OAuth2Provider) int {
		if a == b {
			return 0
		} else if natural.Less(a, b) {
			return -1
		} else {
			return 1
		}
	})
	return r, nil
}, 0)

func InitProvider(ctx context.Context) (err error) {
	logOur := log.StandardLogger().Writer()
	logLevle := hclog.Info
	if flags.Global.Dev {
		logLevle = hclog.Debug
	}
	for _, op := range conf.Conf.Oauth2Plugins {
		op.PluginFile, err = utils.OptFilePath(op.PluginFile)
		if err != nil {
			log.Fatalf("oauth2 plugin file path error: %v", err)
			return err
		}
		log.Infof("load oauth2 plugin: %s", op.PluginFile)
		err := os.MkdirAll(filepath.Dir(op.PluginFile), 0o755)
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

	for _, pi := range providers.AllProvider() {
		InitProviderSetting(pi)
	}

	for _, api := range aggregations.AllAggregation() {
		InitAggregationSetting(api)
	}
	return nil
}

func InitProviderSetting(pi provider.Provider) {
	group := model.SettingGroup(fmt.Sprintf("%s_%s", model.SettingGroupOauth2, pi.Provider()))
	groupSettings := &ProviderGroupSetting{}
	ProviderGroupSettings[group] = groupSettings

	groupSettings.Enabled = settings.NewBoolSetting(fmt.Sprintf("%s_enabled", group), false, group,
		settings.WithBeforeInitBool(func(bs settings.BoolSetting, b bool) (bool, error) {
			defer Oauth2EnabledCache.Refresh(context.Background())
			if b {
				return b, providers.EnableProvider(pi.Provider())
			} else {
				providers.DisableProvider(pi.Provider())
				return b, nil
			}
		}),
		settings.WithInitPriorityBool(1),
		settings.WithBeforeSetBool(func(bs settings.BoolSetting, b bool) (bool, error) {
			defer Oauth2EnabledCache.Refresh(context.Background())
			if b {
				return b, providers.EnableProvider(pi.Provider())
			} else {
				providers.DisableProvider(pi.Provider())
				return b, nil
			}
		}),
	)

	opt := provider.Oauth2Option{}

	groupSettings.ClientID = settings.NewStringSetting(fmt.Sprintf("%s_client_id", group), opt.ClientID, group,
		settings.WithBeforeInitString(func(ss settings.StringSetting, s string) (string, error) {
			opt.ClientID = s
			pi.Init(opt)
			return s, nil
		}),
		settings.WithInitPriorityString(1),
		settings.WithBeforeSetString(func(ss settings.StringSetting, s string) (string, error) {
			opt.ClientID = s
			pi.Init(opt)
			return s, nil
		}))

	groupSettings.ClientSecret = settings.NewStringSetting(fmt.Sprintf("%s_client_secret", group), opt.ClientSecret, group,
		settings.WithBeforeInitString(func(ss settings.StringSetting, s string) (string, error) {
			opt.ClientSecret = s
			pi.Init(opt)
			return s, nil
		}),
		settings.WithInitPriorityString(1),
		settings.WithBeforeSetString(func(ss settings.StringSetting, s string) (string, error) {
			opt.ClientSecret = s
			pi.Init(opt)
			return s, nil
		}))

	groupSettings.RedirectURL = settings.NewStringSetting(fmt.Sprintf("%s_redirect_url", group), opt.RedirectURL, group,
		settings.WithBeforeInitString(func(ss settings.StringSetting, s string) (string, error) {
			opt.RedirectURL = s
			pi.Init(opt)
			return s, nil
		}),
		settings.WithInitPriorityString(1),
		settings.WithBeforeSetString(func(ss settings.StringSetting, s string) (string, error) {
			opt.RedirectURL = s
			pi.Init(opt)
			return s, nil
		}))

	groupSettings.DisableUserSignup = settings.NewBoolSetting(fmt.Sprintf("%s_disable_user_signup", group), false, group)

	groupSettings.SignupNeedReview = settings.NewBoolSetting(fmt.Sprintf("%s_signup_need_review", group), false, group)
}

func InitAggregationProviderSetting(pi provider.Provider) {
	group := model.SettingGroup(fmt.Sprintf("%s_%s", model.SettingGroupOauth2, pi.Provider()))
	groupSettings := &ProviderGroupSetting{}
	ProviderGroupSettings[group] = groupSettings

	groupSettings.Enabled = settings.LoadOrNewBoolSetting(fmt.Sprintf("%s_enabled", group), false, group,
		settings.WithBeforeSetBool(func(bs settings.BoolSetting, b bool) (bool, error) {
			defer Oauth2EnabledCache.Refresh(context.Background())
			if b {
				return b, providers.EnableProvider(pi.Provider())
			} else {
				providers.DisableProvider(pi.Provider())
				return b, nil
			}
		}),
	)

	opt := provider.Oauth2Option{}

	groupSettings.ClientID = settings.LoadOrNewStringSetting(fmt.Sprintf("%s_client_id", group), opt.ClientID, group)
	opt.ClientID = groupSettings.ClientID.Get()
	groupSettings.ClientID.SetBeforeSet(func(ss settings.StringSetting, s string) (string, error) {
		opt.ClientID = s
		pi.Init(opt)
		return s, nil
	})

	groupSettings.ClientSecret = settings.LoadOrNewStringSetting(fmt.Sprintf("%s_client_secret", group), opt.ClientSecret, group)
	opt.ClientSecret = groupSettings.ClientSecret.Get()
	groupSettings.ClientSecret.SetBeforeSet(func(ss settings.StringSetting, s string) (string, error) {
		opt.ClientSecret = s
		pi.Init(opt)
		return s, nil
	})

	groupSettings.RedirectURL = settings.LoadOrNewStringSetting(fmt.Sprintf("%s_redirect_url", group), opt.RedirectURL, group)
	opt.RedirectURL = groupSettings.RedirectURL.Get()
	groupSettings.RedirectURL.SetBeforeSet(func(ss settings.StringSetting, s string) (string, error) {
		opt.RedirectURL = s
		pi.Init(opt)
		return s, nil
	})

	pi.Init(opt)

	groupSettings.DisableUserSignup = settings.LoadOrNewBoolSetting(fmt.Sprintf("%s_disable_user_signup", group), false, group)

	groupSettings.SignupNeedReview = settings.LoadOrNewBoolSetting(fmt.Sprintf("%s_signup_need_review", group), false, group)
}

func InitAggregationSetting(pi provider.AggregationProviderInterface) {
	group := model.SettingGroup(fmt.Sprintf("%s_%s", model.SettingGroupOauth2, pi.Provider()))

	switch pi := pi.(type) {
	case *aggregations.Rainbow:
		settings.NewStringSetting(fmt.Sprintf("%s_api", group), aggregations.DefaultRainbowApi, group,
			settings.WithBeforeInitString(func(ss settings.StringSetting, s string) (string, error) {
				pi.SetAPI(s)
				return s, nil
			},
			),
			settings.WithBeforeSetString(func(ss settings.StringSetting, s string) (string, error) {
				pi.SetAPI(s)
				return s, nil
			},
			),
		)
	}

	list := settings.NewStringSetting(fmt.Sprintf("%s_enabled_list", group), "", group,
		settings.WithBeforeInitString(func(ss settings.StringSetting, s string) (string, error) {
			return s, nil
		}),
		settings.WithInitPriorityString(1),
		settings.WithBeforeSetString(func(ss settings.StringSetting, s string) (string, error) {
			if s == "" {
				return s, nil
			}
			list := strings.Split(s, ",")
			for _, p := range list {
				if slices.Index(pi.Providers(), p) == -1 {
					return s, fmt.Errorf("provider %s not found", p)
				}
			}
			return s, nil
		}),
	)

	settings.NewBoolSetting(fmt.Sprintf("%s_enabled", group), false, group,
		settings.WithBeforeInitBool(func(bs settings.BoolSetting, b bool) (bool, error) {
			if b {
				s := list.Get()
				if s == "" {
					log.Warnf("aggregation provider %s enabled, but no provider enabled", pi.Provider())
				}
				all := pi.Providers()
				list := strings.Split(s, ",")
				enabled := make([]provider.OAuth2Provider, 0, len(list))
				for _, p := range list {
					if slices.Index(all, p) != -1 {
						enabled = append(enabled, p)
					} else {
						log.Warnf("aggregation provider %s enabled, but provider %s not found", pi.Provider(), p)
					}
				}

				pi2, err := provider.ExtractProviders(pi, enabled...)
				if err != nil {
					log.Errorf("aggregation provider %s enabled, but extract provider failed: %s", pi.Provider(), err)
					return b, nil
				}
				for _, pi2 := range pi2 {
					providers.RegisterProvider(pi2)
					InitAggregationProviderSetting(pi2)
				}
			}
			return b, nil
		}),
		settings.WithBeforeSetBool(func(bs settings.BoolSetting, b bool) (bool, error) {
			if len(list.Get()) == 0 {
				return b, fmt.Errorf("enabled provider list is empty")
			}
			return b, nil
		}),
	)
}
