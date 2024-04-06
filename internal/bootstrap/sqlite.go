//go:build !cgo
// +build !cgo

package bootstrap

import (
	"fmt"
	"strings"

	"github.com/glebarez/sqlite"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/utils"
	"gorm.io/gorm"
)

func newSqliteDialector(dbConf conf.DatabaseConfig) gorm.Dialector {
	var dsn string
	if dbConf.CustomDSN != "" {
		dsn = dbConf.CustomDSN
	} else if dbConf.Name == "memory" || strings.HasPrefix(dbConf.Name, ":memory:") {
		dsn = "file::memory:?cache=shared&_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)"
		log.Infof("sqlite3 database memory")
	} else {
		if !strings.HasSuffix(dbConf.Name, ".db") {
			dbConf.Name = dbConf.Name + ".db"
		}
		var err error
		dbConf.Name, err = utils.OptFilePath(dbConf.Name)
		if err != nil {
			log.Fatalf("sqlite3 database file path error: %v", err)
		}
		dsn = fmt.Sprintf("%s?_journal_mode=WAL&_vacuum=incremental&_pragma=foreign_keys(1)", dbConf.Name)
		log.Infof("sqlite3 database file: %s", dbConf.Name)
	}
	return sqlite.Open(dsn)
}
