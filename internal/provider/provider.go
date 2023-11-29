package provider

import (
	"context"

	"golang.org/x/oauth2"
)

type OAuth2Provider string

type UserInfo struct {
	Username       string
	ProviderUserID string
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
	RefreshToken(context.Context, string) (*oauth2.Token, error)
	GetUserInfo(context.Context, *oauth2.Token) (*UserInfo, error)
}
