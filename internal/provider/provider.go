package provider

import (
	"context"
)

type OAuth2Provider = string

type UserInfo struct {
	Username       string
	ProviderUserID string
}

type Oauth2Option struct {
	ClientID     string
	ClientSecret string
	RedirectURL  string
}

type Provider interface {
	Init(opt Oauth2Option)
	Provider() OAuth2Provider
}

type RegistSetting interface {
	RegistSetting(group string)
}

type Interface interface {
	Provider
	NewAuthURL(ctx context.Context, state string) (string, error)
	GetUserInfo(ctx context.Context, code string) (*UserInfo, error)
}
