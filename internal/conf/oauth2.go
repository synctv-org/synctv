package conf

type Oauth2Plugins []struct {
	PluginFile string   `yaml:"plugin_file"`
	Args       []string `yaml:"args"`
}

func DefaultOauth2Plugins() Oauth2Plugins {
	return nil
}
