package conf

type DatabaseType string

const (
	DatabaseTypeSqlite3  DatabaseType = "sqlite3"
	DatabaseTypeMysql    DatabaseType = "mysql"
	DatabaseTypePostgres DatabaseType = "postgres"
)

//nolint:tagliatelle
type DatabaseConfig struct {
	Type     DatabaseType `env:"DATABASE_TYPE"     hc:"support sqlite3, mysql, postgres"                                                                      lc:"default: sqlite3" yaml:"type"`
	Host     string       `env:"DATABASE_HOST"     hc:"when type is not sqlite3, and port is 0, it will use unix socket file"                                 yaml:"host"`
	Port     uint16       `env:"DATABASE_PORT"     yaml:"port"`
	User     string       `env:"DATABASE_USER"     yaml:"user"`
	Password string       `env:"DATABASE_PASSWORD" yaml:"password"`
	Name     string       `env:"DATABASE_NAME"     hc:"when type is sqlite3, it will use sqlite db file or memory"                                            lc:"default: synctv"  yaml:"name"`
	SslMode  string       `env:"DATABASE_SSL_MODE" hc:"mysql: true, false, skip-verify, preferred, <name> postgres: disable, require, verify-ca, verify-full" yaml:"ssl_mode"`

	CustomDSN string `env:"DATABASE_CUSTOM_DSN" hc:"when not empty, it will ignore other config" yaml:"custom_dsn"`

	MaxIdleConns    int    `env:"DATABASE_MAX_IDLE_CONNS"     hc:"sqlite3 does not support setting connection parameters" yaml:"max_idle_conns"`
	MaxOpenConns    int    `env:"DATABASE_MAX_OPEN_CONNS"     yaml:"max_open_conns"`
	ConnMaxLifetime string `env:"DATABASE_CONN_MAX_LIFETIME"  yaml:"conn_max_lifetime"`
	ConnMaxIdleTime string `env:"DATABASE_CONN_MAX_IDLE_TIME" yaml:"conn_max_idle_time"`
}

func DefaultDatabaseConfig() DatabaseConfig {
	return DatabaseConfig{
		Type: DatabaseTypeSqlite3,
		Host: "",
		Name: "synctv",

		MaxIdleConns:    4,
		MaxOpenConns:    64,
		ConnMaxLifetime: "2h",
		ConnMaxIdleTime: "30m",
	}
}
