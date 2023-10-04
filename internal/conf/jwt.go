package conf

import (
	"github.com/synctv-org/synctv/utils"
)

type JwtConfig struct {
	Secret string `yaml:"secret" lc:"jwt secret (default rand string)" env:"JWT_SECRET"`
	Expire int    `yaml:"expire" lc:"expire time (default: 12 hour)" env:"JWT_EXPIRE"`
}

func DefaultJwtConfig() JwtConfig {
	return JwtConfig{
		Secret: utils.RandString(32),
		Expire: 12,
	}
}
