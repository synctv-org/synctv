package db

import (
	"errors"
	"fmt"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/utils"
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

func WithAppendProvider(p provider.OAuth2Provider, puid string) CreateUserConfig {
	return func(u *model.User) {
		u.UserProviders = append(u.UserProviders, model.UserProvider{
			Provider:       p,
			ProviderUserID: puid,
		})
	}
}

func WithSetProvider(p provider.OAuth2Provider, puid string) CreateUserConfig {
	return func(u *model.User) {
		u.UserProviders = []model.UserProvider{{
			Provider:       p,
			ProviderUserID: puid,
		}}
	}
}

func WithAppendProviders(providers []model.UserProvider) CreateUserConfig {
	return func(u *model.User) {
		u.UserProviders = append(u.UserProviders, providers...)
	}
}

func WithSetProviders(providers []model.UserProvider) CreateUserConfig {
	return func(u *model.User) {
		u.UserProviders = providers
	}
}

func WithRegisteredByProvider(b bool) CreateUserConfig {
	return func(u *model.User) {
		u.RegisteredByProvider = b
	}
}

func WithEmail(email string) CreateUserConfig {
	return func(u *model.User) {
		u.Email = email
	}
}

func WithRegisteredByEmail(b bool) CreateUserConfig {
	return func(u *model.User) {
		u.RegisteredByEmail = b
	}
}

func CreateUserWithHashedPassword(username string, hashedPassword []byte, conf ...CreateUserConfig) (*model.User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	if len(hashedPassword) == 0 {
		return nil, errors.New("password cannot be empty")
	}
	u := &model.User{
		Username:       username,
		Role:           model.RoleUser,
		HashedPassword: hashedPassword,
	}
	for _, c := range conf {
		c(u)
	}
	if u.Role == 0 {
		return nil, errors.New("role cannot be empty")
	}
	err := db.Create(u).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return u, errors.New("user already exists")
	}
	return u, err
}

func CreateUser(username string, password string, conf ...CreateUserConfig) (*model.User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	if password == "" {
		return nil, errors.New("password cannot be empty")
	}
	hashedPassword, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
	if err != nil {
		return nil, err
	}
	return CreateUserWithHashedPassword(username, hashedPassword, conf...)
}

func CreateOrLoadUser(username string, password string, conf ...CreateUserConfig) (*model.User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	var user model.User
	if err := db.Where("username = ?", username).First(&user).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return CreateUser(username, password, conf...)
		} else {
			return nil, err
		}
	}
	return &user, nil
}

func CreateOrLoadUserWithHashedPassword(username string, hashedPassword []byte, conf ...CreateUserConfig) (*model.User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	var user model.User
	if err := db.Where("username = ?", username).First(&user).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return CreateUserWithHashedPassword(username, hashedPassword, conf...)
		} else {
			return nil, err
		}
	}
	return &user, nil
}

// 只有当provider和puid没有找到对应的user时才会创建
func CreateOrLoadUserWithProvider(username, password string, p provider.OAuth2Provider, puid string, conf ...CreateUserConfig) (*model.User, error) {
	if puid == "" {
		return nil, errors.New("provider user id cannot be empty")
	}
	var user model.User
	if err := db.Where("id = (?)", db.Table("user_providers").Where("provider = ? AND provider_user_id = ?", p, puid).Select("user_id")).First(&user).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return CreateUser(username, password, append(conf, WithSetProvider(p, puid), WithRegisteredByProvider(true))...)
		} else {
			return nil, err
		}
	} else {
		return &user, nil
	}
}

func CreateOrLoadUserWithEmail(username, password, email string, conf ...CreateUserConfig) (*model.User, error) {
	if email == "" {
		return nil, errors.New("email cannot be empty")
	}
	var user model.User
	if err := db.Where("email = ?", email).First(&user).Error; err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			return CreateUser(username, password, append(conf, WithEmail(email), WithRegisteredByEmail(true))...)
		} else {
			return nil, err
		}
	}
	return &user, nil
}

func CreateUserWithEmail(username, password, email string, conf ...CreateUserConfig) (*model.User, error) {
	if email == "" {
		return nil, errors.New("email cannot be empty")
	}
	return CreateUser(username, password, append(conf, WithEmail(email), WithRegisteredByEmail(true))...)
}

func GetUserByProvider(p provider.OAuth2Provider, puid string) (*model.User, error) {
	var user model.User
	err := db.Where("id = (?)", db.Table("user_providers").Where("provider = ? AND provider_user_id = ?", p, puid).Select("user_id")).First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func GetUserByEmail(email string) (*model.User, error) {
	var user model.User
	err := db.Where("email = ?", email).First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func GetProviderUserID(p provider.OAuth2Provider, puid string) (string, error) {
	var userProvider model.UserProvider
	err := db.Where("provider = ? AND provider_user_id = ?", p, puid).Select("user_id").First(&userProvider).Error
	return userProvider.UserID, HandleNotFound(err, "user")
}

func BindProvider(uid string, p provider.OAuth2Provider, puid string) error {
	err := db.Create(&model.UserProvider{
		UserID:         uid,
		Provider:       p,
		ProviderUserID: puid,
	}).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return errors.New("provider already bind")
	}
	return err
}

// 当用户是通过provider注册的时候，则最少保留一个provider，否则禁止解除绑定
func UnBindProvider(uid string, p provider.OAuth2Provider) error {
	return Transactional(func(tx *gorm.DB) error {
		user := model.User{}
		if err := tx.Scopes(PreloadUserProviders()).Where("id = ?", uid).First(&user).Error; err != nil {
			return HandleNotFound(err, "user")
		}
		if user.RegisteredByProvider && len(user.UserProviders) == 1 {
			return errors.New("user must have at least one provider")
		}
		if err := tx.Where("user_id = ? AND provider = ?", uid, p).Delete(&model.UserProvider{}).Error; err != nil {
			return HandleNotFound(err, "provider")
		}
		return nil
	})
}

func GetBindProviders(uid string) ([]*model.UserProvider, error) {
	var providers []*model.UserProvider
	err := db.Where("user_id = ?", uid).Find(&providers).Error
	return providers, HandleNotFound(err, "user")
}

func GetUserByUsername(username string) (*model.User, error) {
	u := &model.User{}
	err := db.Where("username = ?", username).First(u).Error
	return u, HandleNotFound(err, "user")
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

func GerUsersIDByIDLike(id string, scopes ...func(*gorm.DB) *gorm.DB) []string {
	var ids []string
	db.Model(&model.User{}).Where(`id LIKE ?`, utils.LIKE(id)).Scopes(scopes...).Pluck("id", &ids)
	return ids
}

func GetUserByIDOrUsernameLike(idOrUsername string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.User, error) {
	var users []*model.User
	err := db.Where("id = ? OR username LIKE ?", idOrUsername, fmt.Sprintf("%%%s%%", idOrUsername)).Scopes(scopes...).Find(&users).Error
	return users, HandleNotFound(err, "user")
}

func GetUserByID(id string) (*model.User, error) {
	if len(id) != 32 {
		return nil, errors.New("user id is not 32 bit")
	}
	u := &model.User{}
	err := db.Where("id = ?", id).First(u).Error
	return u, HandleNotFound(err, "user")
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
	return HandleNotFound(err, "user")
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
	return HandleNotFound(err, "user")
}

func DeleteUserByID(userID string) error {
	err := db.Unscoped().Where("id = ?", userID).Delete(&model.User{}).Error
	return HandleNotFound(err, "user")
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
	return db.Omit("created_at").Save(u).Error
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
	return HandleNotFound(err, "user")
}

func RemoveAdminByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser).Error
	return HandleNotFound(err, "user")
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
	return HandleNotFound(err, "user")
}

func RemoveRootByID(userID string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser).Error
	return HandleNotFound(err, "user")
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
	return HandleNotFound(err, "user")
}

func SetUsernameByID(userID string, username string) error {
	err := db.Model(&model.User{}).Where("id = ?", userID).Update("username", username).Error
	return HandleNotFound(err, "user")
}

func GetAllUserCount(scopes ...func(*gorm.DB) *gorm.DB) int64 {
	var count int64
	db.Model(&model.User{}).Scopes(scopes...).Count(&count)
	return count
}

func GetAllUsers(scopes ...func(*gorm.DB) *gorm.DB) []*model.User {
	var users []*model.User
	db.Scopes(scopes...).Find(&users)
	return users
}

func SetUserHashedPassword(id string, hashedPassword []byte) error {
	err := db.Model(&model.User{}).Where("id = ?", id).Update("hashed_password", hashedPassword).Error
	return HandleNotFound(err, "user")
}

func BindEmail(id string, email string) error {
	err := db.Model(&model.User{}).Where("id = ?", id).Update("email", email).Error
	return HandleNotFound(err, "user")
}

func UnbindEmail(uid string) error {
	return Transactional(func(tx *gorm.DB) error {
		user := model.User{}
		if err := tx.Where("id = ?", uid).First(&user).Error; err != nil {
			return HandleNotFound(err, "user")
		}
		if user.Email == "" {
			return errors.New("user has no email")
		}
		if user.RegisteredByEmail {
			return errors.New("user must have one email")
		}
		return tx.Model(&model.User{}).Where("id = ?", uid).Update("email", "").Error
	})
}
