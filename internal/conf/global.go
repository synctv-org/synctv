package conf

type GlobalConfig struct {
	GitHubBaseURL string `yaml:"github_base_url" lc:"default: https://api.github.com/" env:"GITHUB_BASE_URL"`
}

func DefaultGlobalConfig() GlobalConfig {
	return GlobalConfig{
		GitHubBaseURL: "https://api.github.com/",
	}
}
