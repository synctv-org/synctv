package conf

import (
	"github.com/synctv-org/synctv/utils"
)

type JwtConfig struct {
	Secret string `yaml:"secret" env:"JWT_SECRET"`
	Expire string `yaml:"expire" env:"JWT_EXPIRE"`
}

func DefaultJwtConfig() JwtConfig {
	return JwtConfig{
		Secret: utils.RandString(32),
		Expire: "12h",
	}
}
