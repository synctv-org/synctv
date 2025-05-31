package conf

//nolint:tagliatelle
type ServerConfig struct {
	HTTP           HTTPServerConfig `yaml:"http"`
	RTMP           RTMPServerConfig `yaml:"rtmp"`
	ProxyCachePath string           `yaml:"proxy_cache_path" env:"SERVER_PROXY_CACHE_PATH" hc:"proxy cache path storage path, empty means use memory cache"`
	ProxyCacheSize string           `yaml:"proxy_cache_size" env:"SERVER_PROXY_CACHE_SIZE" hc:"proxy cache max size, example: 1MB 1GB, default 1GB"`
}

//nolint:tagliatelle
type HTTPServerConfig struct {
	Listen string `env:"SERVER_LISTEN" yaml:"listen"`
	Port   uint16 `env:"SERVER_PORT"   yaml:"port"`

	CertPath string `env:"SERVER_CERT_PATH" yaml:"cert_path"`
	KeyPath  string `env:"SERVER_KEY_PATH"  yaml:"key_path"`
}

type RTMPServerConfig struct {
	Enable bool   `env:"RTMP_ENABLE" yaml:"enable"`
	Listen string `env:"RTMP_LISTEN" yaml:"listen" lc:"default use http listen"`
	Port   uint16 `env:"RTMP_PORT"   yaml:"port"   lc:"default use server port"`
}

func DefaultServerConfig() ServerConfig {
	return ServerConfig{
		HTTP: HTTPServerConfig{
			Listen:   "0.0.0.0",
			Port:     8080,
			CertPath: "",
			KeyPath:  "",
		},
		RTMP: RTMPServerConfig{
			Enable: true,
			Port:   0,
		},
		ProxyCachePath: "",
	}
}
