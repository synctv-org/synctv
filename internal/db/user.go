package db

import (
	"errors"
	"fmt"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
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
		if err := db.Where("id = ?", userProvider.UserID).First(&user).Error; err != nil {
			return nil, err
		}
	}

	return &user, nil
}

func GetProviderUserID(p provider.OAuth2Provider, puid uint) (string, error) {
	var userProvider model.UserProvider
	if err := db.Where("provider = ? AND provider_user_id = ?", p, puid).First(&userProvider).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return "", errors.New("user not found")
		} else {
			return "", err
		}
	}
	return userProvider.UserID, nil
}

func AddUserToRoom(userID, roomID string, role model.RoomRole, permission model.Permission) error {
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

func GerUsersIDByUsernameLike(username string, scopes ...func(*gorm.DB) *gorm.DB) []string {
	var ids []string
	db.Model(&model.User{}).Where(`username LIKE ?`, fmt.Sprintf("%%%s%%", username)).Scopes(scopes...).Pluck("id", &ids)
	return ids
}

func GetUserByIDOrUsernameLike(idOrUsername string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.User, error) {
	var users []*model.User
	err := db.Where("id = ? OR username LIKE ?", idOrUsername, fmt.Sprintf("%%%s%%", idOrUsername)).Scopes(scopes...).Find(&users).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return users, errors.New("user not found")
	}
	return users, err
}

func GetUserByID(id string) (*model.User, error) {
	if len(id) != 36 {
		return nil, errors.New("user id is not 32 bit")
	}
	u := &model.User{}
	err := db.Where("id = ?", id).First(u).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return u, errors.New("user not found")
	}
	return u, err
}

func BanUser(u *model.User) error {
	if u.Role == model.RoleBanned {
		return nil
	}
	u.Role = model.RoleBanned
	return SaveUser(u)
}

func BanUserByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleBanned).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func UnbanUser(u *model.User) error {
	if u.Role != model.RoleBanned {
		return errors.New("user is not banned")
	}
	u.Role = model.RoleUser
	return SaveUser(u)
}

func UnbanUserByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func DeleteUserByID(userID string) error {
	err := db.Unscoped().Where("id = ?", userID).Delete(&model.User{}).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func LoadAndDeleteUserByID(userID string, columns ...clause.Column) (*model.User, error) {
	u := &model.User{}
	if db.Unscoped().
		Clauses(clause.Returning{Columns: columns}).
		Delete(u, userID).
		RowsAffected == 0 {
		return u, errors.New("user not found")
	}
	return u, nil
}

func SaveUser(u *model.User) error {
	return db.Save(u).Error
}

func AddAdmin(u *model.User) error {
	if u.Role >= model.RoleAdmin {
		return nil
	}
	u.Role = model.RoleAdmin
	return SaveUser(u)
}

func RemoveAdmin(u *model.User) error {
	if u.Role < model.RoleAdmin {
		return nil
	}
	u.Role = model.RoleUser
	return SaveUser(u)
}

func GetAdmins() []*model.User {
	var users []*model.User
	db.Where("role == ?", model.RoleAdmin).Find(&users)
	return users
}

func AddAdminByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleAdmin).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func RemoveAdminByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func AddRoot(u *model.User) error {
	if u.Role == model.RoleRoot {
		return nil
	}
	u.Role = model.RoleRoot
	return SaveUser(u)
}

func RemoveRoot(u *model.User) error {
	if u.Role != model.RoleRoot {
		return nil
	}
	u.Role = model.RoleUser
	return SaveUser(u)
}

func AddRootByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleRoot).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func RemoveRootByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func GetRoots() []*model.User {
	var users []*model.User
	db.Where("role = ?", model.RoleRoot).Find(&users)
	return users
}

func SetRole(u *model.User, role model.Role) error {
	u.Role = role
	return SaveUser(u)
}

func SetRoleByID(userID string, role model.Role) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", role).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}

func GetAllUserWithRoleUser(role model.Role, scopes ...func(*gorm.DB) *gorm.DB) []*model.User {
	users := []*model.User{}
	db.Where("role = ?", role).Scopes(scopes...).Find(&users)
	return users
}

func GetAllUserCountWithRole(role model.Role, scopes ...func(*gorm.DB) *gorm.DB) int64 {
	var count int64
	db.Model(&model.User{}).Where("role = ?", role).Scopes(scopes...).Count(&count)
	return count
}

func SetUsernameByID(userID string, username string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("username", username).Error
	if errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("user not found")
	}
	return err
}
