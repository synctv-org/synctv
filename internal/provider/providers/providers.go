package providers

import (
	"fmt"

	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/gencontainer/rwmap"
)

var (
	enabledProviders rwmap.RWMap[provider.OAuth2Provider, provider.ProviderInterface]
	allProviders     = make(map[provider.OAuth2Provider]provider.ProviderInterface)
)

func InitProvider(p provider.OAuth2Provider, c provider.Oauth2Option) (provider.ProviderInterface, error) {
	pi, ok := allProviders[p]
	if !ok {
		return nil, FormatErrNotImplemented(p)
	}
	pi.Init(c)
	return pi, nil
}

func RegisterProvider(ps ...provider.ProviderInterface) {
	for _, p := range ps {
		allProviders[p.Provider()] = p
	}
}

func GetProvider(p provider.OAuth2Provider) (provider.ProviderInterface, error) {
	pi, ok := enabledProviders.Load(p)
	if !ok {
		return nil, FormatErrNotImplemented(p)
	}
	return pi, nil
}

func AllProvider() map[provider.OAuth2Provider]provider.ProviderInterface {
	return allProviders
}

func EnabledProvider() *rwmap.RWMap[provider.OAuth2Provider, provider.ProviderInterface] {
	return &enabledProviders
}

func EnableProvider(p provider.OAuth2Provider) error {
	pi, ok := allProviders[p]
	if !ok {
		return FormatErrNotImplemented(p)
	}
	enabledProviders.Store(p, pi)
	return nil
}

func DisableProvider(p provider.OAuth2Provider) {
	enabledProviders.Delete(p)
}

type FormatErrNotImplemented string

func (f FormatErrNotImplemented) Error() string {
	return fmt.Sprintf("%s not implemented", string(f))
}
