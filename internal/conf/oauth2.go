package conf

type OAuth2Config []Oauth2Plugin

type Oauth2Plugin struct {
	PluginFile string   `yaml:"plugin_file"`
	Args       []string `yaml:"args"`
}

func DefaultOAuth2Config() OAuth2Config {
	return nil
}
