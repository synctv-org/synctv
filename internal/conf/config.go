package conf

import (
	"github.com/synctv-org/synctv/utils"
)

//nolint:tagliatelle
type Config struct {
	// Log
	Log LogConfig `yaml:"log"`

	// Server
	Server ServerConfig `yaml:"server"`

	// Jwt
	Jwt JwtConfig `yaml:"jwt"`

	// Database
	Database DatabaseConfig `yaml:"database"`

	// Oauth2Plugins
	Oauth2Plugins Oauth2Plugins `yaml:"oauth2_plugins"`

	// RateLimit
	RateLimit RateLimitConfig `yaml:"rate_limit"`
}

func (c *Config) Save(file string) error {
	return utils.WriteYaml(file, c)
}

func DefaultConfig() *Config {
	return &Config{
		// Log
		Log: DefaultLogConfig(),

		// Server
		Server: DefaultServerConfig(),

		// Jwt
		Jwt: DefaultJwtConfig(),

		// Database
		Database: DefaultDatabaseConfig(),

		// OAuth2
		Oauth2Plugins: DefaultOauth2Plugins(),

		// RateLimit
		RateLimit: DefaultRateLimitConfig(),
	}
}
