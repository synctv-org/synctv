package provider

import (
	"context"
	"fmt"

	"golang.org/x/oauth2"
)

type OAuth2Provider string

var (
	enabledProviders map[OAuth2Provider]ProviderInterface
	allowedProviders = make(map[OAuth2Provider]ProviderInterface)
)

type UserInfo struct {
	Username       string
	ProviderUserID uint
}

type ProviderInterface interface {
	Init(ClientID, ClientSecret string)
	Provider() OAuth2Provider
	NewConfig() *oauth2.Config
	GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error)
}

func InitProvider(p OAuth2Provider, ClientID, ClientSecret string) error {
	pi, ok := allowedProviders[p]
	if !ok {
		return FormatErrNotImplemented(p)
	}
	pi.Init(ClientID, ClientSecret)
	if enabledProviders == nil {
		enabledProviders = make(map[OAuth2Provider]ProviderInterface)
	}
	enabledProviders[pi.Provider()] = pi
	return nil
}

func registerProvider(ps ...ProviderInterface) {
	for _, p := range ps {
		allowedProviders[p.Provider()] = p
	}
}

func GetProvider(p OAuth2Provider) (ProviderInterface, error) {
	pi, ok := enabledProviders[p]
	if !ok {
		return nil, FormatErrNotImplemented(p)
	}
	return pi, nil
}

func AllowedProvider() map[OAuth2Provider]ProviderInterface {
	return allowedProviders
}

func EnabledProvider() map[OAuth2Provider]ProviderInterface {
	return enabledProviders
}

type FormatErrNotImplemented string

func (f FormatErrNotImplemented) Error() string {
	return fmt.Sprintf("%s not implemented", string(f))
}
