package conf

type VendorConfig struct {
	Bilibili Bilibili `yaml:"bilibili"`
}

func DefaultVendorConfig() VendorConfig {
	return VendorConfig{
		Bilibili: DefaultBilibiliConfig(),
	}
}

type Consul struct {
	Endpoint string `yaml:"endpoint"`
}

type Etcd struct {
	Endpoints []string `yaml:"endpoints"`
	Username  string   `yaml:"username"`
	Password  string   `yaml:"password"`
}

type Bilibili struct {
	ServerName   string `yaml:"server_name" env:"BILIBILI_SERVER_NAME"`
	Endpoint     string `yaml:"endpoint" env:"BILIBILI_ENDPOINT"`
	JwtSecret    string `yaml:"jwt_secret" env:"BILIBILI_JWT_SECRET"`
	Scheme       string `yaml:"scheme" lc:"grpc | http" env:"BILIBILI_SCHEME"`
	Tls          bool   `yaml:"tls" env:"BILIBILI_TLS"`
	CustomCAFile string `yaml:"custom_ca_file" env:"BILIBILI_CUSTOM_CA_FILE"`
	TimeOut      string `yaml:"time_out" env:"BILIBILI_TIME_OUT"`

	Consul Consul `yaml:"consul,omitempty"`
	Etcd   Etcd   `yaml:"etcd,omitempty"`
}

func DefaultBilibiliConfig() Bilibili {
	return Bilibili{
		ServerName: "bilibili",
		Scheme:     "grpc",
		TimeOut:    "5s",
	}
}
