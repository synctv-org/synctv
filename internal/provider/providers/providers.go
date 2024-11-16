package providers

import (
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/gencontainer/rwmap"
)

var (
	enabledProviders rwmap.RWMap[provider.OAuth2Provider, struct{}]
	allProviders     rwmap.RWMap[provider.OAuth2Provider, provider.Interface]
)

func InitProvider(p provider.OAuth2Provider, c provider.Oauth2Option) (provider.Interface, error) {
	pi, ok := allProviders.Load(p)
	if !ok {
		return nil, FormatNotImplementedError(p)
	}
	pi.Init(c)
	return pi, nil
}

func RegisterProvider(ps ...provider.Interface) {
	for _, p := range ps {
		allProviders.Store(p.Provider(), p)
	}
}

func GetProvider(p provider.OAuth2Provider) (provider.Interface, error) {
	_, ok := enabledProviders.Load(p)
	if !ok {
		return nil, FormatNotImplementedError(p)
	}
	pi, ok := allProviders.Load(p)
	if !ok {
		return nil, FormatNotImplementedError(p)
	}
	return pi, nil
}

func AllProvider() map[provider.OAuth2Provider]provider.Interface {
	m := make(map[provider.OAuth2Provider]provider.Interface)
	allProviders.Range(func(key string, value provider.Interface) bool {
		m[key] = value
		return true
	})
	return m
}

func EnabledProvider() *rwmap.RWMap[provider.OAuth2Provider, struct{}] {
	return &enabledProviders
}

func EnableProvider(p provider.OAuth2Provider) error {
	_, ok := allProviders.Load(p)
	if !ok {
		return FormatNotImplementedError(p)
	}
	enabledProviders.Store(p, struct{}{})
	return nil
}

func DisableProvider(p provider.OAuth2Provider) error {
	_, ok := allProviders.Load(p)
	if !ok {
		return FormatNotImplementedError(p)
	}
	enabledProviders.Delete(p)
	return nil
}

type FormatNotImplementedError string

func (f FormatNotImplementedError) Error() string {
	return string(f) + " is not implemented"
}
