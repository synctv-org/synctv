package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

type CreateUserConfig func(u *model.User)

func WithRole(role model.Role) CreateUserConfig {
	return func(u *model.User) {
		u.Role = role
	}
}

func CreateUser(username string, hashedPassword []byte, conf ...CreateUserConfig) (*model.User, error) {
	u := &model.User{
		Username:       username,
		HashedPassword: hashedPassword,
		Role:           model.RoleUser,
	}
	for _, c := range conf {
		c(u)
	}
	err := db.Create(u).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return u, errors.New("username already exists")
	}
	return u, err
}

func AddUserToRoom(userID uint, roomID uint, role model.RoomRole, permission model.Permission) error {
	ur := &model.RoomUserRelation{
		UserID:      userID,
		RoomID:      roomID,
		Role:        role,
		Permissions: permission,
	}
	err := db.Create(ur).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return errors.New("user already exists in room")
	}
	return err
}

func GetUserByUsername(username string) (*model.User, error) {
	u := &model.User{}
	err := db.Where("username = ?", username).First(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func GetUserByID(id uint) (*model.User, error) {
	u := &model.User{}
	err := db.Where("id = ?", id).First(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func GetUsersByRoomID(roomID uint) ([]model.User, error) {
	users := []model.User{}
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ?", roomID).Find(&users).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return users, errors.New("room not found")
	}
	return users, err
}

func DeleteUserByID(userID uint) error {
	err := db.Unscoped().Delete(&model.User{}, userID).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func LoadAndDeleteUserByID(userID uint, columns ...clause.Column) (*model.User, error) {
	u := &model.User{}
	err := db.Unscoped().
		Clauses(clause.Returning{Columns: columns}).
		Delete(u, userID).
		Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func DeleteUserByUsername(username string) error {
	err := db.Unscoped().Where("username = ?", username).Delete(&model.User{}).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func LoadAndDeleteUserByUsername(username string, columns ...clause.Column) (*model.User, error) {
	u := &model.User{}
	err := db.Unscoped().Clauses(clause.Returning{Columns: columns}).Where("username = ?", username).Delete(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func SetUserPassword(userID uint, password string) error {
	var hashedPassword []byte
	if password != "" {
		var err error
		hashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
	}
	return SetUserHashedPassword(userID, hashedPassword)
}

func SetUserHashedPassword(userID uint, hashedPassword []byte) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("hashed_password", hashedPassword).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func SaveUser(u *model.User) error {
	return db.Save(u).Error
}
