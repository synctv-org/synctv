//go:build cgo
// +build cgo

package bootstrap

import (
	"gorm.io/driver/sqlite"
	"gorm.io/gorm"
)

func openSqlite(dsn string) gorm.Dialector {
	return sqlite.Open(dsn)
}
