package bootstrap

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"

	"github.com/glebarez/sqlite"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/utils"
	"gorm.io/driver/mysql"
	"gorm.io/driver/postgres"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"
)

func InitDatabase(ctx context.Context) error {
	var dialector gorm.Dialector
	var opts []gorm.Option
	switch conf.Conf.Database.Type {
	case conf.DatabaseTypeMysql:
		var dsn string
		if conf.Conf.Database.CustomDSN != "" {
			dsn = conf.Conf.Database.CustomDSN
		} else if conf.Conf.Database.Port == 0 {
			dsn = fmt.Sprintf("%s:%s@unix(%s)/%s?charset=utf8mb4&parseTime=True&loc=Local&tls=%s&interpolateParams=true",
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.Host,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
			log.Infof("mysql database unix socket: %s", conf.Conf.Database.Host)
		} else {
			dsn = fmt.Sprintf("%s:%s@tcp(%s:%d)/%s?charset=utf8mb4&parseTime=True&loc=Local&tls=%s&interpolateParams=true",
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.Host,
				conf.Conf.Database.Port,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
			log.Infof("mysql database tcp: %s:%d", conf.Conf.Database.Host, conf.Conf.Database.Port)
		}
		dialector = mysql.New(mysql.Config{
			DSN:                       dsn,
			DefaultStringSize:         256,
			DisableDatetimePrecision:  true,
			DontSupportRenameIndex:    true,
			DontSupportRenameColumn:   true,
			SkipInitializeWithVersion: false,
		})
		// opts = append(opts, &gorm.Config{})
	case conf.DatabaseTypeSqlite3:
		var dsn string
		if conf.Conf.Database.CustomDSN != "" {
			dsn = conf.Conf.Database.CustomDSN
		} else if conf.Conf.Database.DBName == "memory" || strings.HasPrefix(conf.Conf.Database.DBName, ":memory:") {
			dsn = "file::memory:?cache=shared&_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)"
			log.Infof("sqlite3 database memory")
		} else {
			if !strings.HasSuffix(conf.Conf.Database.DBName, ".db") {
				conf.Conf.Database.DBName = conf.Conf.Database.DBName + ".db"
			}
			utils.OptFilePath(&conf.Conf.Database.DBName)
			dsn = fmt.Sprintf("%s?_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)", conf.Conf.Database.DBName)
			log.Infof("sqlite3 database file: %s", conf.Conf.Database.DBName)
		}
		dialector = sqlite.Open(dsn)
		// opts = append(opts, &gorm.Config{})
	case conf.DatabaseTypePostgres:
		var dsn string
		if conf.Conf.Database.CustomDSN != "" {
			dsn = conf.Conf.Database.CustomDSN
		} else if conf.Conf.Database.Port == 0 {
			dsn = fmt.Sprintf("host=%s user=%s password=%s dbname=%s sslmode=%s",
				conf.Conf.Database.Host,
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
			log.Infof("postgres database unix socket: %s", conf.Conf.Database.Host)
		} else {
			dsn = fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=%s sslmode=%s",
				conf.Conf.Database.Host,
				conf.Conf.Database.Port,
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
			log.Infof("postgres database tcp: %s:%d", conf.Conf.Database.Host, conf.Conf.Database.Port)
		}
		dialector = postgres.New(postgres.Config{
			DSN:                  dsn,
			PreferSimpleProtocol: true,
		})
		// opts = append(opts, &gorm.Config{})
	default:
		log.Fatalf("unknown database type: %s", conf.Conf.Database.Type)
	}
	opts = append(opts, &gorm.Config{
		TranslateError: true,
		Logger:         newDBLogger(),
		PrepareStmt:    true,
	})
	d, err := gorm.Open(dialector, opts...)
	if err != nil {
		log.Fatalf("failed to connect database: %s", err.Error())
	}
	sqlDB, err := d.DB()
	if err != nil {
		log.Fatalf("failed to get sqlDB: %s", err.Error())
	}
	initRawDB(sqlDB)
	return db.Init(d, conf.Conf.Database.Type)
}

func newDBLogger() logger.Interface {
	var logLevel logger.LogLevel
	if flags.Dev {
		logLevel = logger.Info
	} else {
		logLevel = logger.Warn
	}
	return logger.New(
		log.StandardLogger(),
		logger.Config{
			SlowThreshold:             time.Second,
			LogLevel:                  logLevel,
			IgnoreRecordNotFoundError: true,
			ParameterizedQueries:      !flags.Dev,
			Colorful:                  true,
		},
	)
}

func initRawDB(db *sql.DB) {
	db.SetMaxOpenConns(conf.Conf.Database.MaxOpenConns)
	db.SetMaxIdleConns(conf.Conf.Database.MaxIdleConns)
	d, err := time.ParseDuration(conf.Conf.Database.ConnMaxLifetime)
	if err != nil {
		log.Fatalf("failed to parse conn_max_lifetime: %s", err.Error())
	}
	db.SetConnMaxLifetime(d)
}
