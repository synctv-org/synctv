package conf

import (
	"github.com/synctv-org/synctv/internal/provider"
)

type OAuth2Config map[provider.OAuth2Provider]OAuth2ProviderConfig

type OAuth2ProviderConfig struct {
	ClientID     string `yaml:"client_id"`
	ClientSecret string `yaml:"client_secret"`
}

func DefaultOAuth2Config() OAuth2Config {
	return OAuth2Config{
		(&provider.GithubProvider{}).Provider(): {
			ClientID:     "github_client_id",
			ClientSecret: "github_client_secret",
		},
	}
}
