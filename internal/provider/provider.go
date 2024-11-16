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
	Init(Oauth2Option)
	Provider() OAuth2Provider
}

type Interface interface {
	Provider
	NewAuthURL(context.Context, string) (string, error)
	GetUserInfo(context.Context, string) (*UserInfo, error)
}
