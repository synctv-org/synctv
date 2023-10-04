package conf

type ProxyConfig struct {
	MovieProxy bool `yaml:"movie_proxy" lc:"enable movie proxy (default: true)" env:"PROXY_MOVIE_PROXY"`
	LiveProxy  bool `yaml:"live_proxy" lc:"enable live proxy (default: true)" env:"PROXY_LIVE_PROXY"`
}

func DefaultProxyConfig() ProxyConfig {
	return ProxyConfig{
		MovieProxy: true,
		LiveProxy:  true,
	}
}
