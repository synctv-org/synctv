package conf

type ProxyConfig struct {
	MovieProxy bool `yaml:"movie_proxy" env:"PROXY_MOVIE"`
	LiveProxy  bool `yaml:"live_proxy" env:"PROXY_LIVE"`
}

func DefaultProxyConfig() ProxyConfig {
	return ProxyConfig{
		MovieProxy: true,
		LiveProxy:  true,
	}
}
