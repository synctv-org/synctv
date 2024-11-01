package conf

type ServerConfig struct {
	Http           HttpServerConfig `yaml:"http"`
	Rtmp           RtmpServerConfig `yaml:"rtmp"`
	ProxyCachePath string           `yaml:"proxy_cache_path" env:"SERVER_PROXY_CACHE_PATH"`
}

type HttpServerConfig struct {
	Listen string `yaml:"listen" env:"SERVER_LISTEN"`
	Port   uint16 `yaml:"port" env:"SERVER_PORT"`
	Quic   bool   `yaml:"quic" hc:"enable http3/quic need set cert and key file" env:"SERVER_QUIC"`

	CertPath string `yaml:"cert_path" env:"SERVER_CERT_PATH"`
	KeyPath  string `yaml:"key_path" env:"SERVER_KEY_PATH"`
}

type RtmpServerConfig struct {
	Enable bool   `yaml:"enable" env:"RTMP_ENABLE"`
	Listen string `yaml:"listen" lc:"default use http listen" env:"RTMP_LISTEN"`
	Port   uint16 `yaml:"port" lc:"default use server port" env:"RTMP_PORT"`
}

func DefaultServerConfig() ServerConfig {
	return ServerConfig{
		Http: HttpServerConfig{
			Listen:   "0.0.0.0",
			Port:     8080,
			Quic:     true,
			CertPath: "",
			KeyPath:  "",
		},
		Rtmp: RtmpServerConfig{
			Enable: true,
			Port:   0,
		},
		ProxyCachePath: "",
	}
}
