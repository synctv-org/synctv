package conf

type LogConfig struct {
	Enable     bool   `yaml:"enable" lc:"enable log to file (default: true)" env:"LOG_ENABLE"`
	LogFormat  string `yaml:"log_format" lc:"log format, can be set: text | json (default: text)" env:"LOG_FORMAT"`
	FilePath   string `yaml:"file_path" lc:"log file path (default: log/log.log)" env:"LOG_FILE_PATH"`
	MaxSize    int    `yaml:"max_size" lc:"max size per log file (default: 10 megabytes)" env:"LOG_MAX_SIZE"`
	MaxBackups int    `yaml:"max_backups" lc:"max backups (default: 10)" env:"LOG_MAX_BACKUPS"`
	MaxAge     int    `yaml:"max_age" lc:"max age (default: 28 days)" env:"LOG_MAX_AGE"`
	Compress   bool   `yaml:"compress" lc:"compress (default: false)" env:"LOG_COMPRESS"`
}

func DefaultLogConfig() LogConfig {
	return LogConfig{
		Enable:     true,
		LogFormat:  "text",
		FilePath:   "log/log.log",
		MaxSize:    10,
		MaxBackups: 10,
		MaxAge:     28,
		Compress:   false,
	}
}
