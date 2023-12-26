package db

import (
	"fmt"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

type dbVersion struct {
	NextVersion string
	Upgrade     func(*gorm.DB) error
}

const CurrentVersion = "0.0.2"

var models = []any{
	new(model.Setting),
	new(model.User),
	new(model.UserProvider),
	new(model.Room),
	new(model.RoomUserRelation),
	new(model.Movie),
	new(model.BilibiliVendor),
	new(model.AlistVendor),
	new(model.EmbyVendor),
	new(model.VendorBackend),
}

var dbVersions = map[string]dbVersion{
	"0.0.1": {
		NextVersion: "0.0.2",
		Upgrade: func(db *gorm.DB) error {
			return db.Migrator().DropTable("streaming_vendor_infos")
		},
	},
	"0.0.2": {
		NextVersion: "",
		Upgrade:     nil,
	},
}

func UpgradeDatabase() error {
	var currentVersion string
	if db.Migrator().HasTable(&model.Setting{}) {
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
		if currentVersion != CurrentVersion {
			err = autoMigrate(models...)
			if err != nil {
				return err
			}
		}
	} else {
		err := autoMigrate(models...)
		if err != nil {
			return err
		}
		currentVersion = CurrentVersion
	}

	version, ok := dbVersions[currentVersion]
	if !ok {
		return nil
	}
	currentVersion = version.NextVersion
	for currentVersion != "" {
		version, ok := dbVersions[currentVersion]
		if !ok {
			break
		}
		log.Infof("Upgrading database to version %s", currentVersion)
		if version.Upgrade != nil {
			err := version.Upgrade(db)
			if err != nil {
				return err
			}
		}
		err := UpdateSettingItemValue("database_version", currentVersion)
		if err != nil {
			return err
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
		if err := db.Set("gorm:table_options", "ENGINE=InnoDB CHARSET=utf8mb4").AutoMigrate(dst...); err != nil {
			return err
		}
		return db.Exec("SET FOREIGN_KEY_CHECKS = 1").Error
	case conf.DatabaseTypeSqlite3, conf.DatabaseTypePostgres:
		return db.AutoMigrate(dst...)
	default:
		return fmt.Errorf("unknown database type: %s", conf.Conf.Database.Type)
	}
}
