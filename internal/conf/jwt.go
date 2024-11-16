package conf

import (
	"github.com/synctv-org/synctv/utils"
)

type JwtConfig struct {
	Secret string `env:"JWT_SECRET" yaml:"secret"`
	Expire string `env:"JWT_EXPIRE" yaml:"expire"`
}

func DefaultJwtConfig() JwtConfig {
	return JwtConfig{
		Secret: utils.RandString(32),
		Expire: "48h",
	}
}
