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

func WithID(id string) CreateUserConfig {
	return func(u *model.User) {
		u.ID = id
	}
}

func WithRole(role model.Role) CreateUserConfig {
	return func(u *model.User) {
		u.Role = role
	}
}

func WithRegisteredByEmail(email string) CreateUserConfig {
	return func(u *model.User) {
		u.Email = model.EmptyNullString(email)
		u.RegisteredByEmail = true
	}
}

func WithEnableAutoAddUsernameSuffix() CreateUserConfig {
	return func(u *model.User) {
		u.EnableAutoAddUsernameSuffix()
	}
}

func WithDisableAutoAddUsernameSuffix() CreateUserConfig {
	return func(u *model.User) {
		u.DisableAutoAddUsernameSuffix()
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
	if u.RegisteredByEmail && u.Email.String() == "" {
		return nil, errors.New("email cannot be empty")
	}
	if u.Role == 0 {
		return nil, errors.New("role cannot be empty")
	}
	err := db.Create(u).Error
	if err != nil {
		if errors.Is(err, gorm.ErrDuplicatedKey) {
			return u, errors.New("user already exists")
		}
		return nil, fmt.Errorf("failed to create user: %w", err)
	}
	return u, nil
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
		return nil, fmt.Errorf("failed to hash password: %w", err)
	}
	return CreateUserWithHashedPassword(username, hashedPassword, conf...)
}

func CreateOrLoadUserWithProvider(username, password string, p provider.OAuth2Provider, puid string, conf ...CreateUserConfig) (*model.User, error) {
	if puid == "" {
		return nil, errors.New("provider user id cannot be empty")
	}
	hashedPassword, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
	if err != nil {
		return nil, fmt.Errorf("failed to hash password: %w", err)
	}
	user := &model.User{
		Username:       username,
		HashedPassword: hashedPassword,
		Role:           model.RoleUser,
		UserProviders: []*model.UserProvider{{
			Provider:       p,
			ProviderUserID: puid,
		}},
		RegisteredByProvider: true,
	}
	if user.Role == 0 {
		return nil, errors.New("role cannot be empty")
	}
	for _, c := range conf {
		c(user)
	}
	user.EnableAutoAddUsernameSuffix()
	err = OnConflictDoNothing().Joins("JOIN user_providers ON users.id = user_providers.user_id").
		Where("user_providers.provider = ? AND user_providers.provider_user_id = ?", p, puid).
		FirstOrCreate(user).Error
	if err != nil {
		return nil, fmt.Errorf("failed to create or load user: %w", err)
	}
	return user, nil
}

func CreateUserWithEmail(username, password, email string, conf ...CreateUserConfig) (*model.User, error) {
	if email == "" {
		return nil, errors.New("email cannot be empty")
	}
	return CreateUser(username, password, append(conf,
		WithRegisteredByEmail(email),
		WithEnableAutoAddUsernameSuffix(),
	)...)
}

func GetUserByProvider(p provider.OAuth2Provider, puid string) (*model.User, error) {
	var user model.User
	err := db.Joins("JOIN user_providers ON users.id = user_providers.user_id").
		Where("user_providers.provider = ? AND user_providers.provider_user_id = ?", p, puid).
		First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func GetUserByEmail(email string) (*model.User, error) {
	var user model.User
	err := db.Where("email = ?", email).First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func GetProviderUserID(p provider.OAuth2Provider, puid string) (string, error) {
	var userID string
	err := db.Model(&model.UserProvider{}).
		Where("provider = ? AND provider_user_id = ?", p, puid).
		Select("user_id").
		First(&userID).Error
	return userID, HandleNotFound(err, "user")
}

func BindProvider(uid string, p provider.OAuth2Provider, puid string) error {
	err := db.Create(&model.UserProvider{
		UserID:         uid,
		Provider:       p,
		ProviderUserID: puid,
	}).Error
	if err != nil {
		if errors.Is(err, gorm.ErrDuplicatedKey) {
			return errors.New("provider already bound")
		}
		return fmt.Errorf("failed to bind provider: %w", err)
	}
	return nil
}

func UnBindProvider(uid string, p provider.OAuth2Provider) error {
	return Transactional(func(tx *gorm.DB) error {
		var user model.User
		if err := tx.Preload("UserProviders").Where("id = ?", uid).First(&user).Error; err != nil {
			return HandleNotFound(err, "user")
		}
		if user.RegisteredByProvider && len(user.UserProviders) <= 1 {
			return errors.New("user must have at least one provider")
		}
		result := tx.Where("user_id = ? AND provider = ?", uid, p).Delete(&model.UserProvider{})
		return HandleUpdateResult(result, "provider")
	})
}

func BindEmail(id string, email string) error {
	result := db.Model(&model.User{}).Where("id = ?", id).Update("email", model.EmptyNullString(email))
	return HandleUpdateResult(result, "user")
}

func UnbindEmail(uid string) error {
	return Transactional(func(tx *gorm.DB) error {
		var user model.User
		if err := tx.Select("email", "registered_by_email").Where("id = ?", uid).First(&user).Error; err != nil {
			return HandleNotFound(err, "user")
		}
		if user.RegisteredByEmail {
			return errors.New("user must have one email")
		}
		if user.Email.String() == "" {
			return nil
		}
		result := tx.Model(&model.User{}).Where("id = ?", uid).Update("email", model.EmptyNullString(""))
		return HandleUpdateResult(result, "user")
	})
}

func GetBindProviders(uid string) ([]*model.UserProvider, error) {
	var providers []*model.UserProvider
	err := db.Where("user_id = ?", uid).Find(&providers).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get bind providers: %w", err)
	}
	return providers, nil
}

func GetUserByUsername(username string) (*model.User, error) {
	var user model.User
	err := db.Where("username = ?", username).First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func GetUserByUsernameLike(username string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.User, error) {
	var users []*model.User
	err := db.Where("username LIKE ?", fmt.Sprintf("%%%s%%", username)).Scopes(scopes...).Find(&users).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get users by username like: %w", err)
	}
	return users, nil
}

func GerUsersIDByUsernameLike(username string, scopes ...func(*gorm.DB) *gorm.DB) ([]string, error) {
	var ids []string
	err := db.Model(&model.User{}).Where("username LIKE ?", fmt.Sprintf("%%%s%%", username)).Scopes(scopes...).Pluck("id", &ids).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get user IDs by username like: %w", err)
	}
	return ids, nil
}

func GerUsersIDByIDLike(id string, scopes ...func(*gorm.DB) *gorm.DB) ([]string, error) {
	var ids []string
	err := db.Model(&model.User{}).Where("id LIKE ?", utils.LIKE(id)).Scopes(scopes...).Pluck("id", &ids).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get user IDs by ID like: %w", err)
	}
	return ids, nil
}

func GetUserByIDOrUsernameLike(idOrUsername string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.User, error) {
	var users []*model.User
	err := db.Where("id = ? OR username LIKE ?", idOrUsername, fmt.Sprintf("%%%s%%", idOrUsername)).Scopes(scopes...).Find(&users).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get users by ID or username like: %w", err)
	}
	return users, nil
}

func GetUserByID(id string) (*model.User, error) {
	if len(id) != 32 {
		return nil, errors.New("user id is not 32 bit")
	}
	var user model.User
	err := db.Where("id = ?", id).First(&user).Error
	return &user, HandleNotFound(err, "user")
}

func BanUser(u *model.User) error {
	if u.Role == model.RoleBanned {
		return nil
	}
	u.Role = model.RoleBanned
	return SaveUser(u)
}

func BanUserByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleBanned)
	return HandleUpdateResult(result, "user")
}

func UnbanUser(u *model.User) error {
	if u.Role != model.RoleBanned {
		return errors.New("user is not banned")
	}
	u.Role = model.RoleUser
	return SaveUser(u)
}

func UnbanUserByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser)
	return HandleUpdateResult(result, "user")
}

func DeleteUserByID(userID string) error {
	result := db.Unscoped().Select(clause.Associations).Delete(&model.User{ID: userID})
	return HandleUpdateResult(result, "user")
}

func LoadAndDeleteUserByID(userID string, columns ...clause.Column) (*model.User, error) {
	var user model.User
	result := db.Unscoped().
		Clauses(clause.Returning{Columns: columns}).
		Select(clause.Associations).
		Where("id = ?", userID).
		Delete(&user)
	return &user, HandleNotFound(result.Error, "user")
}

func SaveUser(u *model.User) error {
	result := db.Omit("created_at").Save(u)
	return HandleUpdateResult(result, "user")
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

func GetAdmins() ([]*model.User, error) {
	var users []*model.User
	err := db.Where("role = ?", model.RoleAdmin).Find(&users).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get admins: %w", err)
	}
	return users, nil
}

func AddAdminByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleAdmin)
	return HandleUpdateResult(result, "user")
}

func RemoveAdminByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser)
	return HandleUpdateResult(result, "user")
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
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleRoot)
	return HandleUpdateResult(result, "user")
}

func RemoveRootByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser)
	return HandleUpdateResult(result, "user")
}

func GetRoots() []*model.User {
	var users []*model.User
	db.Where("role = ?", model.RoleRoot).Find(&users)
	return users
}

func SetAdminRoleByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleAdmin)
	return HandleUpdateResult(result, "user")
}

func SetRootRoleByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleRoot)
	return HandleUpdateResult(result, "user")
}

func SetUserRoleByID(userID string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("role", model.RoleUser)
	return HandleUpdateResult(result, "user")
}

func SetUsernameByID(userID string, username string) error {
	result := db.Model(&model.User{}).Where("id = ?", userID).Update("username", username)
	return HandleUpdateResult(result, "user")
}

func GetUserCount(scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64
	err := db.Model(&model.User{}).Scopes(scopes...).Count(&count).Error
	if err != nil {
		return 0, fmt.Errorf("failed to get user count: %w", err)
	}
	return count, nil
}

func GetUsers(scopes ...func(*gorm.DB) *gorm.DB) ([]*model.User, error) {
	var users []*model.User
	err := db.Scopes(scopes...).Find(&users).Error
	if err != nil {
		return nil, fmt.Errorf("failed to get users: %w", err)
	}
	return users, nil
}

func SetUserHashedPassword(id string, hashedPassword []byte) error {
	result := db.Model(&model.User{}).Where("id = ?", id).Update("hashed_password", hashedPassword)
	return HandleUpdateResult(result, "user")
}
