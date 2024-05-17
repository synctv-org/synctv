package flags

type ServerFlags struct {
	SkipConfig         bool   `env:"SKIP_CONFIG"`
	SkipEnvConfig      bool   `env:"SKIP_ENV_CONFIG"`
	DisableUpdateCheck bool   `env:"DISABLE_UPDATE_CHECK"`
	DisableWeb         bool   `env:"DISABLE_WEB"`
	WebPath            string `env:"WEB_PATH"`
	DisableLogColor    bool   `env:"DISABLE_LOG_COLOR"`
}
