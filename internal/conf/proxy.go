package conf

type ProxyConfig struct {
	MovieProxy bool `yaml:"movie_proxy" lc:"enable movie proxy (default: true)" env:"PROXY_MOVIE"`
	LiveProxy  bool `yaml:"live_proxy" lc:"enable live proxy (default: true)" env:"PROXY_LIVE"`
}

func DefaultProxyConfig() ProxyConfig {
	return ProxyConfig{
		MovieProxy: true,
		LiveProxy:  true,
	}
}
