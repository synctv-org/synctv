package bootstrap

import (
	"context"
	"database/sql"
	"fmt"
	"strings"
	"time"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/version"
	"github.com/synctv-org/synctv/utils"
	"gorm.io/driver/mysql"
	"gorm.io/driver/postgres"
	"gorm.io/gorm"
	"gorm.io/gorm/logger"
)

func InitDatabase(ctx context.Context) (err error) {
	dialector, err := createDialector(conf.Conf.Database)
	if err != nil {
		log.Fatalf("failed to create dialector: %s", err.Error())
	}

	var opts []gorm.Option
	opts = append(opts, &gorm.Config{
		TranslateError:                           true,
		Logger:                                   newDBLogger(),
		PrepareStmt:                              true,
		DisableForeignKeyConstraintWhenMigrating: false,
		IgnoreRelationshipsWhenMigrating:         false,
	})
	d, err := gorm.Open(dialector, opts...)
	if err != nil {
		log.Fatalf("failed to connect database: %s", err.Error())
	}
	sqlDB, err := d.DB()
	if err != nil {
		log.Fatalf("failed to get sqlDB: %s", err.Error())
	}
	if conf.Conf.Database.Type != conf.DatabaseTypeSqlite3 {
		initRawDB(sqlDB)
	}
	return db.Init(d, conf.Conf.Database.Type)
}

func createDialector(dbConf conf.DatabaseConfig) (dialector gorm.Dialector, err error) {
	var dsn string
	switch dbConf.Type {
	case conf.DatabaseTypeMysql:
		if dbConf.CustomDSN != "" {
			dsn = dbConf.CustomDSN
		} else if dbConf.Port == 0 {
			dsn = fmt.Sprintf("%s:%s@unix(%s)/%s?charset=utf8mb4&parseTime=True&loc=Local&interpolateParams=true&tls=%s",
				dbConf.User,
				dbConf.Password,
				dbConf.Host,
				dbConf.Name,
				dbConf.SslMode,
			)
			log.Infof("mysql database: %s", dbConf.Host)
		} else {
			dsn = fmt.Sprintf("%s:%s@tcp(%s:%d)/%s?charset=utf8mb4&parseTime=True&loc=Local&interpolateParams=true&tls=%s",
				dbConf.User,
				dbConf.Password,
				dbConf.Host,
				dbConf.Port,
				dbConf.Name,
				dbConf.SslMode,
			)
			log.Infof("mysql database tcp: %s:%d", dbConf.Host, dbConf.Port)
		}
		dialector = mysql.New(mysql.Config{
			DSN:                       dsn,
			DefaultStringSize:         256,
			DisableDatetimePrecision:  true,
			DontSupportRenameIndex:    true,
			DontSupportRenameColumn:   true,
			SkipInitializeWithVersion: false,
		})
	case conf.DatabaseTypeSqlite3:
		if dbConf.CustomDSN != "" {
			dsn = dbConf.CustomDSN
		} else if dbConf.Name == "memory" || strings.HasPrefix(dbConf.Name, ":memory:") {
			dsn = "file::memory:?cache=shared&_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)"
			log.Infof("sqlite3 database memory")
		} else {
			if !strings.HasSuffix(dbConf.Name, ".db") {
				dbConf.Name = dbConf.Name + ".db"
			}
			dbConf.Name, err = utils.OptFilePath(dbConf.Name)
			if err != nil {
				log.Fatalf("sqlite3 database file path error: %v", err)
			}
			dsn = fmt.Sprintf("%s?_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)", dbConf.Name)
			log.Infof("sqlite3 database file: %s", dbConf.Name)
		}
		dialector = openSqlite(dsn)
	case conf.DatabaseTypePostgres:
		if dbConf.CustomDSN != "" {
			dsn = dbConf.CustomDSN
		} else if dbConf.Port == 0 {
			dsn = fmt.Sprintf("host=%s user=%s password=%s dbname=%s sslmode=%s",
				dbConf.Host,
				dbConf.User,
				dbConf.Password,
				dbConf.Name,
				dbConf.SslMode,
			)
			log.Infof("postgres database: %s", dbConf.Host)
		} else {
			dsn = fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=%s sslmode=%s",
				dbConf.Host,
				dbConf.Port,
				dbConf.User,
				dbConf.Password,
				dbConf.Name,
				dbConf.SslMode,
			)
			log.Infof("postgres database tcp: %s:%d", dbConf.Host, dbConf.Port)
		}
		dialector = postgres.New(postgres.Config{
			DSN:                  dsn,
			PreferSimpleProtocol: true,
		})
	default:
		log.Fatalf("unknown database type: %s", dbConf.Type)
	}
	return
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
			ParameterizedQueries:      !flags.Dev && version.Version != "dev",
			Colorful:                  utils.ForceColor(),
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
	d, err = time.ParseDuration(conf.Conf.Database.ConnMaxIdleTime)
	if err != nil {
		log.Fatalf("failed to parse conn_max_idle_time: %s", err.Error())
	}
	db.SetConnMaxIdleTime(d)
}
