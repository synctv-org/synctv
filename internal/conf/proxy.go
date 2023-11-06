package conf

type ProxyConfig struct {
	MovieProxy        bool `yaml:"movie_proxy" env:"PROXY_MOVIE"`
	LiveProxy         bool `yaml:"live_proxy" env:"PROXY_LIVE"`
	AllowProxyToLocal bool `yaml:"allow_proxy_to_local" env:"PROXY_ALLOW_TO_LOCAL"`
}

func DefaultProxyConfig() ProxyConfig {
	return ProxyConfig{
		MovieProxy:        true,
		LiveProxy:         true,
		AllowProxyToLocal: false,
	}
}
