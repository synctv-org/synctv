//go:build !cgo
// +build !cgo

package bootstrap

import (
	"github.com/glebarez/sqlite"
	"gorm.io/gorm"
)

func openSqlite(dsn string) gorm.Dialector {
	return sqlite.Open(dsn)
}
