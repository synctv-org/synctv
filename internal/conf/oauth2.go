package conf

import (
	"github.com/synctv-org/synctv/internal/provider"
)

type OAuth2Config struct {
	Providers map[provider.OAuth2Provider]OAuth2ProviderConfig `yaml:"providers"`
	Plugins   []Oauth2Plugin                                   `yaml:"plugins"`
}

type Oauth2Plugin struct {
	PluginFile string   `yaml:"plugin_file"`
	Arges      []string `yaml:"arges"`
}

type Endpoint struct {
	AuthURL       string `yaml:"auth_url"`
	DeviceAuthURL string `yaml:"device_auth_url"`
	TokenURL      string `yaml:"token_url"`
}

type OAuth2ProviderConfig struct {
	ClientID     string    `yaml:"client_id"`
	ClientSecret string    `yaml:"client_secret"`
	RedirectURL  string    `yaml:"redirect_url"`
	Endpoint     *Endpoint `yaml:"endpoint,omitempty"`
}

func DefaultOAuth2Config() OAuth2Config {
	return OAuth2Config{
		Providers: map[provider.OAuth2Provider]OAuth2ProviderConfig{},
		Plugins:   []Oauth2Plugin{},
	}
}
