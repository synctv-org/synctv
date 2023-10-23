package conf

import (
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/providers"
)

type OAuth2Config struct {
	Providers map[provider.OAuth2Provider]OAuth2ProviderConfig `yaml:"providers"`
	Plugins   []Oauth2Plugin                                   `yaml:"plugins"`
}

type Oauth2Plugin struct {
	PluginFile string   `yaml:"plugin_file"`
	Arges      []string `yaml:"arges"`
}

type OAuth2ProviderConfig struct {
	ClientID     string `yaml:"client_id"`
	ClientSecret string `yaml:"client_secret"`
	RedirectURL  string `yaml:"redirect_url"`
}

func DefaultOAuth2Config() OAuth2Config {
	return OAuth2Config{
		Providers: map[provider.OAuth2Provider]OAuth2ProviderConfig{
			(&providers.GithubProvider{}).Provider(): {
				ClientID:     "",
				ClientSecret: "",
				RedirectURL:  "",
			},
		},
	}
}
