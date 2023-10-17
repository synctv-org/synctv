package bootstrap

import (
	"context"
	"fmt"
	"strings"

	"github.com/glebarez/sqlite"
	log "github.com/sirupsen/logrus"
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
		} else {
			dsn = fmt.Sprintf("%s:%s@tcp(%s:%d)/%s?charset=utf8mb4&parseTime=True&loc=Local&tls=%s",
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.Host,
				conf.Conf.Database.Port,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
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
		if conf.Conf.Database.Host == "memory" || strings.HasPrefix(conf.Conf.Database.Host, ":memory:") {
			dsn = "file::memory:?cache=shared"
		} else if !strings.HasSuffix(conf.Conf.Database.Host, ".db") {
			dsn = fmt.Sprintf("%s.db?_journal_mode=WAL&_vacuum=incremental", conf.Conf.Database.DBName)
		} else {
			dsn = conf.Conf.Database.DBName
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
		} else {
			dsn = fmt.Sprintf("host=%s port=%d user=%s password=%s dbname=%s sslmode=%s",
				conf.Conf.Database.Host,
				conf.Conf.Database.Port,
				conf.Conf.Database.User,
				conf.Conf.Database.Password,
				conf.Conf.Database.DBName,
				conf.Conf.Database.SslMode,
			)
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
