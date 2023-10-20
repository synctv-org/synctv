package conf

import (
	"github.com/synctv-org/synctv/internal/provider"
)

type OAuth2Config map[provider.OAuth2Provider]OAuth2ProviderConfig

type OAuth2ProviderConfig struct {
	ClientID     string `yaml:"client_id" lc:"oauth2 client id"`
	ClientSecret string `yaml:"client_secret" lc:"oauth2 client secret"`
	// CustomRedirectURL string               `yaml:"custom_redirect_url" lc:"oauth2 custom redirect url"`
}

func DefaultOAuth2Config() OAuth2Config {
	return OAuth2Config{
		provider.GithubProvider{}.Provider(): {
			ClientID:     "github_client_id",
			ClientSecret: "github_client_secret",
		},
	}
}
