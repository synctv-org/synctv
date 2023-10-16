package conf

type ServerConfig struct {
	Listen string `yaml:"listen" lc:"server listen addr (default: 0.0.0.0)" env:"SERVER_LISTEN"`
	Port   uint16 `yaml:"port" lc:"server listen port (default: 8080)" env:"SERVER_PORT"`
	Quic   bool   `yaml:"quic" lc:"enable http3/quic, need enable ssl, set cert and key file (default: true)" env:"SERVER_QUIC"`

	CertPath string `yaml:"cert_path" lc:"cert path" env:"SERVER_CERT_PATH"`
	KeyPath  string `yaml:"key_path" lc:"key path" env:"SERVER_KEY_PATH"`
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
