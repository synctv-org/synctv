package conf

type ServerConfig struct {
	Listen string `yaml:"listen" env:"SERVER_LISTEN"`
	Port   uint16 `yaml:"port" env:"SERVER_PORT"`
	Quic   bool   `yaml:"quic" hc:"enable http3/quic need set cert and key file" env:"SERVER_QUIC"`

	CertPath string `yaml:"cert_path" env:"SERVER_CERT_PATH"`
	KeyPath  string `yaml:"key_path" env:"SERVER_KEY_PATH"`
}

func DefaultServerConfig() ServerConfig {
	return ServerConfig{
		Listen:   "0.0.0.0",
		Port:     8080,
		Quic:     true,
		CertPath: "",
		KeyPath:  "",
	}
}
