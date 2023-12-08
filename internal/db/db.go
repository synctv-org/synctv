package db

import (
	"errors"
	"fmt"
	"strings"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/utils"
	_ "github.com/synctv-org/synctv/utils/fastJSONSerializer"
	"gorm.io/gorm"
)

var (
	db     *gorm.DB
	dbType conf.DatabaseType
)

func Init(d *gorm.DB, t conf.DatabaseType) error {
	db = d
	dbType = t
	return AutoMigrate(new(model.Setting), new(model.User), new(model.UserProvider), new(model.Room), new(model.RoomUserRelation), new(model.StreamingVendorInfo), new(model.Movie))
}

func AutoMigrate(dst ...any) error {
	var err error
	switch conf.Conf.Database.Type {
	case conf.DatabaseTypeMysql:
		err = db.Set("gorm:table_options", "ENGINE=InnoDB CHARSET=utf8mb4").AutoMigrate(dst...)
	case conf.DatabaseTypeSqlite3, conf.DatabaseTypePostgres:
		err = db.AutoMigrate(dst...)
	default:
		log.Fatalf("unknown database type: %s", conf.Conf.Database.Type)
	}
	if err != nil {
		return err
	}
	err = initRootUser()
	if err != nil {
		return err
	}
	return upgradeDatabase()
}

func initRootUser() error {
	user := model.User{}
	err := db.Where("role = ?", model.RoleRoot).First(&user).Error
	if err == nil || !errors.Is(err, gorm.ErrRecordNotFound) {
		return err
	}
	u, err := CreateUser("root", "root", WithRole(model.RoleRoot))
	log.Infof("init root user:\nid: %s\nusername: %s\npassword: %s", u.ID, u.Username, "root")
	return err
}

func DB() *gorm.DB {
	return db
}

func Close() {
	log.Info("closing db")
	sqlDB, err := db.DB()
	if err != nil {
		log.Errorf("failed to get db: %s", err.Error())
		return
	}
	err = sqlDB.Close()
	if err != nil {
		log.Errorf("failed to close db: %s", err.Error())
		return
	}
}

func Paginate(page, pageSize int) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		if page <= 0 {
			page = 1
		}
		if pageSize <= 0 {
			pageSize = 10
		}

		offset := (page - 1) * pageSize
		return db.Offset(offset).Limit(pageSize)
	}
}

func OrderByAsc(column string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Order(column + " asc")
	}
}

func OrderByDesc(column string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Order(column + " desc")
	}
}

func OrderByCreatedAtAsc(db *gorm.DB) *gorm.DB {
	return db.Order("created_at asc")
}

func OrderByCreatedAtDesc(db *gorm.DB) *gorm.DB {
	return db.Order("created_at desc")
}

func OrderByIDAsc(db *gorm.DB) *gorm.DB {
	return db.Order("id asc")
}

func OrderByIDDesc(db *gorm.DB) *gorm.DB {
	return db.Order("id desc")
}

func WithUser(db *gorm.DB) *gorm.DB {
	return db.Preload("User")
}

func WhereRoomID(roomID string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("room_id = ?", roomID)
	}
}

func PreloadRoomUserRelations(scopes ...func(*gorm.DB) *gorm.DB) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Preload("RoomUserRelations", func(db *gorm.DB) *gorm.DB {
			return db.Scopes(scopes...)
		})
	}
}

func PreloadUserProviders(scopes ...func(*gorm.DB) *gorm.DB) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Preload("UserProviders", func(db *gorm.DB) *gorm.DB {
			return db.Scopes(scopes...)
		})
	}
}

func WhereUserID(userID string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("user_id = ?", userID)
	}
}

func WhereCreatorID(creatorID string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("creator_id = ?", creatorID)
	}
}

// column cannot be a user parameter
func WhereEqual(column string, value interface{}) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where(fmt.Sprintf("%s = ?", column), value)
	}
}

// column cannot be a user parameter
func WhereLike(column string, value string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where(fmt.Sprintf("%s ILIKE ?", column), utils.LIKE(value))
		default:
			return db.Where(fmt.Sprintf("%s LIKE ?", column), utils.LIKE(value))
		}
	}
}

func WhereRoomNameLikeOrCreatorInOrIDLike(name string, ids []string, id string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("name ILIKE ? OR creator_id IN ? OR id ILIKE ?", utils.LIKE(name), ids, id)
		default:
			return db.Where("name LIKE ? OR creator_id IN ? OR id LIKE ?", utils.LIKE(name), ids, id)
		}
	}
}

func WhereRoomNameLikeOrIDLike(name string, id string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("name ILIKE ? OR id ILIKE ?", utils.LIKE(name), id)
		default:
			return db.Where("name LIKE ? OR id LIKE ?", utils.LIKE(name), id)
		}
	}
}

func WhereRoomNameLike(name string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("name ILIKE ?", utils.LIKE(name))
		default:
			return db.Where("name LIKE ?", utils.LIKE(name))
		}
	}
}

func WhereUsernameLike(name string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("username ILIKE ?", utils.LIKE(name))
		default:
			return db.Where("username LIKE ?", utils.LIKE(name))
		}
	}
}

func WhereCreatorIDIn(ids []string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("creator_id IN ?", ids)
	}
}

func Select(columns ...string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Select(columns)
	}
}

func WhereStatus(status model.RoomStatus) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("status = ?", status)
	}
}

func WhereRole(role model.Role) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("role = ?", role)
	}
}

func WhereUsernameLikeOrIDIn(name string, ids []string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("username ILIKE ? OR id IN ?", utils.LIKE(name), ids)
		default:
			return db.Where("username LIKE ? OR id IN ?", utils.LIKE(name), ids)
		}
	}
}

func WhereIDIn(ids []string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("id IN ?", ids)
	}
}

func WhereRoomSettingWithoutHidden() func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("settings_hidden = ?", false)
	}
}

func WhereRoomSettingHidden() func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("settings_hidden = ?", true)
	}
}

func WhereIDLike(id string) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		switch dbType {
		case conf.DatabaseTypePostgres:
			return db.Where("id ILIKE ?", utils.LIKE(id))
		default:
			return db.Where("id LIKE ?", utils.LIKE(id))
		}
	}
}

func WhereRoomUserStatus(status model.RoomUserStatus) func(db *gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		return db.Where("status = ?", status)
	}
}

type ErrNotFound string

func (e ErrNotFound) Error() string {
	return fmt.Sprintf("%s not found", string(e))
}

func HandleNotFound(err error, errMsg ...string) error {
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return ErrNotFound(strings.Join(errMsg, " "))
	}
	return err
}

func Transactional(txFunc func(*gorm.DB) error) (err error) {
	tx := db.Begin()
	defer func() {
		if err != nil {
			tx.Rollback()
		} else {
			tx.Commit()
		}
	}()
	err = txFunc(tx)
	return
}
