package db

import (
	"errors"
	"fmt"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
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

func CreateUser(username string, p provider.OAuth2Provider, puid uint, conf ...CreateUserConfig) (*model.User, error) {
	u := &model.User{
		Username: username,
		Role:     model.RoleUser,
		Providers: []model.UserProvider{
			{
				Provider:       p,
				ProviderUserID: puid,
			},
		},
	}
	for _, c := range conf {
		c(u)
	}
	err := db.Create(u).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return u, errors.New("user already exists")
	}
	return u, err
}

// 只有当provider和puid没有找到对应的user时才会创建
func CreateOrLoadUser(username string, p provider.OAuth2Provider, puid uint, conf ...CreateUserConfig) (*model.User, error) {
	var user model.User
	var userProvider model.UserProvider

	if err := db.Where("provider = ? AND provider_user_id = ?", p, puid).First(&userProvider).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return CreateUser(username, p, puid, conf...)
		} else {
			return nil, err
		}
	} else {
		if err := db.First(&user, userProvider.UserID).Error; err != nil {
			return nil, err
		}
	}

	return &user, nil
}

func GetUserByProvider(p provider.OAuth2Provider, puid uint) (*model.User, error) {
	u := &model.User{}
	err := db.Preload("Providers", "provider = ? AND provider_user_id = ?", p, puid).First(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
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

func GetUserByUsernameLike(username string, scopes ...func(*gorm.DB) *gorm.DB) []*model.User {
	var users []*model.User
	db.Where(`username LIKE ?`, fmt.Sprintf("%%%s%%", username)).Scopes(scopes...).Find(&users)
	return users
}

func GerUsersIDByUsernameLike(username string, scopes ...func(*gorm.DB) *gorm.DB) []uint {
	var ids []uint
	db.Model(&model.User{}).Where(`username LIKE ?`, fmt.Sprintf("%%%s%%", username)).Scopes(scopes...).Pluck("id", &ids)
	return ids
}

func GetUserByID(id uint) (*model.User, error) {
	u := &model.User{}
	err := db.Where("id = ?", id).First(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func GetUsersByRoomID(roomID uint, scopes ...func(*gorm.DB) *gorm.DB) ([]model.User, error) {
	users := []model.User{}
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ?", roomID).Scopes(scopes...).Find(&users).Error
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
