package providers

import (
	"fmt"

	"github.com/synctv-org/synctv/internal/provider"
)

var (
	enabledProviders map[provider.OAuth2Provider]provider.ProviderInterface
	allowedProviders = make(map[provider.OAuth2Provider]provider.ProviderInterface)
)

func InitProvider(p provider.OAuth2Provider, c provider.Oauth2Option) error {
	pi, ok := allowedProviders[p]
	if !ok {
		return FormatErrNotImplemented(p)
	}
	pi.Init(c)
	if enabledProviders == nil {
		enabledProviders = make(map[provider.OAuth2Provider]provider.ProviderInterface)
	}
	enabledProviders[pi.Provider()] = pi
	return nil
}

func RegisterProvider(ps ...provider.ProviderInterface) {
	for _, p := range ps {
		allowedProviders[p.Provider()] = p
	}
}

func GetProvider(p provider.OAuth2Provider) (provider.ProviderInterface, error) {
	pi, ok := enabledProviders[p]
	if !ok {
		return nil, FormatErrNotImplemented(p)
	}
	return pi, nil
}

func AllowedProvider() map[provider.OAuth2Provider]provider.ProviderInterface {
	return allowedProviders
}

func EnabledProvider() map[provider.OAuth2Provider]provider.ProviderInterface {
	return enabledProviders
}

type FormatErrNotImplemented string

func (f FormatErrNotImplemented) Error() string {
	return fmt.Sprintf("%s not implemented", string(f))
}
