package bootstrap

import (
	"context"
	"fmt"
	"path/filepath"
	"strings"

	"github.com/glebarez/sqlite"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	"gorm.io/driver/mysql"
	"gorm.io/gorm"
)

func InitDatabase(ctx context.Context) error {
	var dialector gorm.Dialector
	var opts []gorm.Option
	switch conf.Conf.Database.Type {
	case conf.DatabaseTypeMysql:
		var dsn string
		if conf.Conf.Database.Port == 0 {
			dsn = fmt.Sprintf("%s:%s@unix(%s)/%s?charset=utf8mb4&parseTime=True&loc=Local&tls=%s",
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.Host,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
			log.Infof("mysql database unix socket: %s", conf.Conf.Database.Host)
		} else {
			dsn = fmt.Sprintf("%s:%s@tcp(%s:%d)/%s?charset=utf8mb4&parseTime=True&loc=Local&tls=%s",
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
		opts = append(opts, &gorm.Config{})
	case conf.DatabaseTypeSqlite3:
		var dsn string
		if conf.Conf.Database.DBName == "memory" || strings.HasPrefix(conf.Conf.Database.DBName, ":memory:") {
			dsn = "file::memory:?cache=shared"
			log.Infof("sqlite3 database memory")
		} else {
			if !strings.HasSuffix(conf.Conf.Database.DBName, ".db") {
				conf.Conf.Database.DBName = conf.Conf.Database.DBName + ".db"
			}
			if !filepath.IsAbs(conf.Conf.Database.DBName) {
				conf.Conf.Database.DBName = filepath.Join(flags.DataDir, conf.Conf.Database.DBName)
			}
			dsn = fmt.Sprintf("%s?_journal_mode=WAL&_vacuum=incremental", conf.Conf.Database.DBName)
			log.Infof("sqlite3 database file: %s", conf.Conf.Database.DBName)
		}
		dialector = sqlite.Open(dsn)
		opts = append(opts, &gorm.Config{})
	case conf.DatabaseTypePostgres:
		var dsn string
		if conf.Conf.Database.Port == 0 {
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
		dialector = mysql.Open(dsn)
		opts = append(opts, &gorm.Config{})
	default:
		log.Fatalf("unknown database type: %s", conf.Conf.Database.Type)
	}
	d, err := gorm.Open(dialector, opts...)
	if err != nil {
		log.Fatalf("failed to connect database: %s", err.Error())
	}
	return db.Init(d)
}
