package db

import (
	"fmt"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

type dbVersion struct {
	Upgrade     func(*gorm.DB) error
	NextVersion string
}

const CurrentVersion = "0.0.12"

var models = []any{
	new(model.Setting),
	new(model.User),
	new(model.UserProvider),
	new(model.Room),
	new(model.RoomSettings),
	new(model.RoomMember),
	new(model.Movie),
	new(model.BilibiliVendor),
	new(model.AlistVendor),
	new(model.EmbyVendor),
	new(model.VendorBackend),
}

var dbVersions = map[string]dbVersion{
	"0.0.1": {
		NextVersion: "0.0.2",
	},
	"0.0.2": {
		NextVersion: "0.0.3",
		Upgrade: func(db *gorm.DB) error {
			// alist and emby movies path are changed, so we need to delete them
			_ = db.Exec("DELETE FROM movies WHERE base_vendor_info_vendor IN ('alist', 'emby')").Error
			_ = db.Migrator().DropTable("alist_vendors", "emby_vendors")
			// delete all vendors, because we are going to change the more vendor table, e.g. bilibili_vendors
			_ = db.Migrator().DropTable("streaming_vendor_infos")
			return autoMigrate(
				new(model.AlistVendor),
				new(model.EmbyVendor),
			)
		},
	},
	"0.0.3": {
		NextVersion: "0.0.4",
	},
	"0.0.4": {
		NextVersion: "0.0.5",
	},
	"0.0.5": {
		NextVersion: "0.0.6",
	},
	"0.0.6": {
		NextVersion: "0.0.7",
		Upgrade: func(d *gorm.DB) error {
			// delete all emby vendors records
			_ = d.Exec("DELETE FROM emby_vendors").Error
			return nil
		},
	},
	"0.0.7": {
		NextVersion: "0.0.8",
	},
	"0.0.8": {
		NextVersion: "0.0.9",
	},
	"0.0.9": {
		NextVersion: "0.0.10",
	},
	"0.0.10": {
		NextVersion: "0.0.11",
	},
	"0.0.11": {
		NextVersion: "0.0.12",
	},
	"0.0.12": {
		NextVersion: "",
	},
}

func UpgradeDatabase() error {
	if !db.Migrator().HasTable(&model.Setting{}) {
		return autoMigrate(models...)
	}
	setting := model.Setting{
		Name:  "database_version",
		Type:  model.SettingTypeString,
		Group: model.SettingGroupDatabase,
		Value: CurrentVersion,
	}
	err := FirstOrCreateSettingItemValue(&setting)
	if err != nil {
		return err
	}
	currentVersion := setting.Value
	log.Infof("current database version: %s", currentVersion)
	if flags.Global.ForceAutoMigrate || currentVersion != CurrentVersion {
		err = autoMigrate(models...)
		if err != nil {
			log.Fatalf("failed to auto migrate database: %s", err.Error())
		}
	}
	for currentVersion != "" {
		version, ok := dbVersions[currentVersion]
		if !ok {
			break
		}
		if version.NextVersion != "" {
			log.Infof("Upgrading database to version %s", version.NextVersion)
			if version.Upgrade != nil {
				err := version.Upgrade(db)
				if err != nil {
					return err
				}
			}
			err := UpdateSettingItemValue("database_version", version.NextVersion)
			if err != nil {
				return err
			}
		}
		currentVersion = version.NextVersion
	}
	return nil
}

func autoMigrate(dst ...any) error {
	log.Info("migrating database...")
	switch conf.Conf.Database.Type {
	case conf.DatabaseTypeMysql:
		if err := db.Exec("SET FOREIGN_KEY_CHECKS = 0").Error; err != nil {
			return err
		}
		defer func() {
			err := db.Exec("SET FOREIGN_KEY_CHECKS = 1").Error
			if err != nil {
				log.Fatalf("failed to set foreign key checks: %s", err.Error())
			}
		}()
		return db.Set("gorm:table_options", "ENGINE=InnoDB CHARSET=utf8mb4").AutoMigrate(dst...)
	case conf.DatabaseTypeSqlite3:
		if err := db.Exec("PRAGMA foreign_keys = OFF").Error; err != nil {
			return err
		}
		defer func() {
			err := db.Exec("PRAGMA foreign_keys = ON").Error
			if err != nil {
				log.Fatalf("failed to set foreign key checks: %s", err.Error())
			}
		}()
		return db.AutoMigrate(dst...)
	case conf.DatabaseTypePostgres:
		if err := db.Exec("SET CONSTRAINTS ALL DEFERRED").Error; err != nil {
			return err
		}
		defer func() {
			err := db.Exec("SET CONSTRAINTS ALL IMMEDIATE").Error
			if err != nil {
				log.Fatalf("failed to set foreign key checks: %s", err.Error())
			}
		}()
		return db.AutoMigrate(dst...)
	default:
		return fmt.Errorf("unknown database type: %s", conf.Conf.Database.Type)
	}
}
