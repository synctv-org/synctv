package provider

import (
	"context"
	"fmt"
	"sync"

	"golang.org/x/oauth2"
)

type OAuth2Provider string

var (
	providers = make(map[OAuth2Provider]ProviderInterface)
	lock      sync.Mutex
)

type UserInfo struct {
	Username       string
	ProviderUserID uint
}

type ProviderInterface interface {
	Provider() OAuth2Provider
	NewConfig(ClientID, ClientSecret string) *oauth2.Config
	GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error)
}

func RegisterProvider(provider ProviderInterface) {
	lock.Lock()
	defer lock.Unlock()
	providers[provider.Provider()] = provider
}

func (p OAuth2Provider) GetProvider() (ProviderInterface, error) {
	lock.Lock()
	defer lock.Unlock()
	pi, ok := providers[p]
	if !ok {
		return nil, FormatErrNotImplemented(p)
	}
	return pi, nil
}

type FormatErrNotImplemented string

func (f FormatErrNotImplemented) Error() string {
	return fmt.Sprintf("%s not implemented", string(f))
}
