package provider

import (
	"context"
	"fmt"

	"golang.org/x/oauth2"
)

type OAuth2Provider string

type TokenRefreshed struct {
	Refreshed bool
	Token     *oauth2.Token
}

type UserInfo struct {
	Username       string
	ProviderUserID uint
	TokenRefreshed *TokenRefreshed
}

type Oauth2Option struct {
	ClientID     string
	ClientSecret string
	RedirectURL  string
}

type ProviderInterface interface {
	Init(Oauth2Option)
	Provider() OAuth2Provider
	NewAuthURL(string) string
	GetToken(context.Context, string) (*oauth2.Token, error)
	GetUserInfo(context.Context, *oauth2.Token) (*UserInfo, error)
}

var (
	enabledProviders map[OAuth2Provider]ProviderInterface
	allowedProviders = make(map[OAuth2Provider]ProviderInterface)
)

func InitProvider(p OAuth2Provider, c Oauth2Option) error {
	pi, ok := allowedProviders[p]
	if !ok {
		return FormatErrNotImplemented(p)
	}
	pi.Init(c)
	if enabledProviders == nil {
		enabledProviders = make(map[OAuth2Provider]ProviderInterface)
	}
	enabledProviders[pi.Provider()] = pi
	return nil
}

func RegisterProvider(ps ...ProviderInterface) {
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
