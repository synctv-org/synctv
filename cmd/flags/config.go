package flags

type GlobalFlags struct {
	Dev              bool   `env:"DEV"`
	LogStd           bool   `env:"LOG_STD"`
	GitHubBaseURL    string `env:"GITHUB_BASE_URL"`
	DataDir          string `env:"DATA_DIR"`
	ForceAutoMigrate bool   `env:"FORCE_AUTO_MIGRATE"`
}
