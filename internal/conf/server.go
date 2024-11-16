package conf

//nolint:tagliatelle
type ServerConfig struct {
	HTTP           HttpServerConfig `yaml:"http"`
	RTMP           RtmpServerConfig `yaml:"rtmp"`
	ProxyCachePath string           `env:"SERVER_PROXY_CACHE_PATH" hc:"proxy cache path storage path, empty means use memory cache" yaml:"proxy_cache_path"`
	ProxyCacheSize string           `env:"SERVER_PROXY_CACHE_SIZE" hc:"proxy cache max size, example: 1MB 1GB, default 1GB"         yaml:"proxy_cache_size"`
}

//nolint:tagliatelle
type HttpServerConfig struct {
	Listen string `env:"SERVER_LISTEN" yaml:"listen"`
	Port   uint16 `env:"SERVER_PORT"   yaml:"port"`

	CertPath string `env:"SERVER_CERT_PATH" yaml:"cert_path"`
	KeyPath  string `env:"SERVER_KEY_PATH"  yaml:"key_path"`
}

type RtmpServerConfig struct {
	Enable bool   `env:"RTMP_ENABLE" yaml:"enable"`
	Listen string `env:"RTMP_LISTEN" lc:"default use http listen" yaml:"listen"`
	Port   uint16 `env:"RTMP_PORT"   lc:"default use server port" yaml:"port"`
}

func DefaultServerConfig() ServerConfig {
	return ServerConfig{
		HTTP: HttpServerConfig{
			Listen:   "0.0.0.0",
			Port:     8080,
			CertPath: "",
			KeyPath:  "",
		},
		RTMP: RtmpServerConfig{
			Enable: true,
			Port:   0,
		},
		ProxyCachePath: "",
	}
}
