package conf

type DatabaseType string

const (
	DatabaseTypeSqlite3  DatabaseType = "sqlite3"
	DatabaseTypeMysql    DatabaseType = "mysql"
	DatabaseTypePostgres DatabaseType = "postgres"
)

type DatabaseConfig struct {
	Type     DatabaseType `yaml:"type" lc:"database type, support sqlite3, mysql, postgres" env:"DATABASE_TYPE"`
	Host     string       `yaml:"host" lc:"database host, when type is not sqlite3, and port is 0, it will use unix socket file" env:"DATABASE_HOST"`
	Port     uint16       `yaml:"port" lc:"database port" env:"DATABASE_PORT"`
	User     string       `yaml:"user" lc:"database user" env:"DATABASE_USER"`
	Password string       `yaml:"password" lc:"database password" env:"DATABASE_PASSWORD"`
	DBName   string       `yaml:"db_name" lc:"database name, when type is sqlite3, it will use sqlite db file or memory" env:"DATABASE_DB_NAME"`
	SslMode  string       `yaml:"ssl_mode" lc:"database ssl mode, default disable" env:"DATABASE_SSL_MODE"`

	CustomDSN string `yaml:"custom_dsn" lc:"custom dsn, when not empty, it will ignore other config" env:"DATABASE_CUSTOM_DSN"`

	MaxIdleConns    int `yaml:"max_idle_conns" lc:"max idle connections (default 10)" env:"DATABASE_MAX_IDLE_CONNS"`
	MaxOpenConns    int `yaml:"max_open_conns" lc:"max open connections (default 100)" env:"DATABASE_MAX_OPEN_CONNS"`
	ConnMaxLifetime int `yaml:"conn_max_lifetime" lc:"connection max lifetime (default 3600 seconds)" env:"DATABASE_CONN_MAX_LIFETIME"`
}

func DefaultDatabaseConfig() DatabaseConfig {
	return DatabaseConfig{
		Type:    DatabaseTypeSqlite3,
		Host:    "",
		DBName:  "synctv",
		SslMode: "disable",

		MaxIdleConns:    10,
		MaxOpenConns:    100,
		ConnMaxLifetime: 3600,
	}
}
