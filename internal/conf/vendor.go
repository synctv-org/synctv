package conf

type VendorConfig struct {
	Bilibili map[string]BilibiliConfig `yaml:"bilibili" hc:"default use local vendor"`
	Alist    map[string]AlistConfig    `yaml:"alist" hc:"default use local vendor"`
}

func DefaultVendorConfig() VendorConfig {
	return VendorConfig{
		Bilibili: nil,
	}
}

type Consul struct {
	Endpoint   string `yaml:"endpoint"`
	Token      string `yaml:"token,omitempty"`
	TokenFile  string `yaml:"token_file,omitempty"`
	PathPrefix string `yaml:"path_prefix,omitempty"`
	Namespace  string `yaml:"namespace,omitempty"`
	Partition  string `yaml:"partition,omitempty"`
}

type Etcd struct {
	Endpoints []string `yaml:"endpoints"`
	Username  string   `yaml:"username,omitempty"`
	Password  string   `yaml:"password,omitempty"`
}

type VendorBase struct {
	ServerName   string `yaml:"server_name" hc:"if use tls and grpc, servername must set the cert server name" env:"BILIBILI_SERVER_NAME"`
	Endpoint     string `yaml:"endpoint" env:"BILIBILI_ENDPOINT"`
	JwtSecret    string `yaml:"jwt_secret" env:"BILIBILI_JWT_SECRET"`
	Scheme       string `yaml:"scheme" lc:"grpc | http" env:"BILIBILI_SCHEME"`
	Tls          bool   `yaml:"tls" env:"BILIBILI_TLS"`
	CustomCAFile string `yaml:"custom_ca_file,omitempty" env:"BILIBILI_CUSTOM_CA_FILE"`
	TimeOut      string `yaml:"time_out" env:"BILIBILI_TIME_OUT"`

	Consul Consul `yaml:"consul,omitempty" hc:"if use consul, must set the endpoint"`
	Etcd   Etcd   `yaml:"etcd,omitempty" hc:"if use etcd, must set the endpoints"`
}

type BilibiliConfig struct {
	VendorBase `yaml:",inline"`
}

type AlistConfig struct {
	VendorBase `yaml:",inline"`
}
