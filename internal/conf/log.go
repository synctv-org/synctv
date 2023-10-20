package conf

type LogConfig struct {
	Enable     bool   `yaml:"enable" env:"LOG_ENABLE"`
	LogFormat  string `yaml:"log_format" hc:"can be set: text | json" env:"LOG_FORMAT"`
	FilePath   string `yaml:"file_path" hc:"if it is a relative path, the data-dir directory will be used." env:"LOG_FILE_PATH"`
	MaxSize    int    `yaml:"max_size" cm:"mb" hc:"max size per log file" env:"LOG_MAX_SIZE"`
	MaxBackups int    `yaml:"max_backups" env:"LOG_MAX_BACKUPS"`
	MaxAge     int    `yaml:"max_age" env:"LOG_MAX_AGE"`
	Compress   bool   `yaml:"compress" env:"LOG_COMPRESS"`
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
