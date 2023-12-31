package op

import (
	"errors"
	"hash/crc32"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/gencontainer/synccache"
)

var userCache *synccache.SyncCache[string, *User]

type UserEntry = synccache.Entry[*User]

var (
	ErrUserBanned  = errors.New("user banned")
	ErrUserPending = errors.New("user pending, please wait for admin to approve")
)

func LoadOrInitUser(u *model.User) (*UserEntry, error) {
	i, _ := userCache.LoadOrStore(u.ID, &User{
		User:    *u,
		version: crc32.ChecksumIEEE(u.HashedPassword),
	}, time.Hour)
	return i, nil
}

func LoadOrInitUserByID(id string) (*UserEntry, error) {
	u, ok := userCache.Load(id)
	if ok {
		u.SetExpiration(time.Now().Add(time.Hour))
		return u, nil
	}

	user, err := db.GetUserByID(id)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(user)
}

func LoadUserByUsername(username string) (*UserEntry, error) {
	u, err := db.GetUserByUsername(username)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CreateOrLoadUser(username string, password string, conf ...db.CreateUserConfig) (*UserEntry, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateOrLoadUser(username, password, conf...)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CreateUser(username string, password string, conf ...db.CreateUserConfig) (*UserEntry, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateUser(username, password, conf...)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CreateOrLoadUserWithProvider(username, password string, p provider.OAuth2Provider, pid string, conf ...db.CreateUserConfig) (*UserEntry, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateOrLoadUserWithProvider(username, password, p, pid, conf...)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func GetUserByProvider(p provider.OAuth2Provider, pid string) (*UserEntry, error) {
	u, err := db.GetUserByProvider(p, pid)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CompareAndDeleteUser(user *UserEntry) error {
	err := db.DeleteUserByID(user.Value().ID)
	if err != nil {
		return err
	}
	return CompareAndCloseUser(user)
}

func DeleteUserByID(id string) error {
	err := db.DeleteUserByID(id)
	if err != nil {
		return err
	}
	return CloseUserById(id)
}

func CloseUserById(id string) error {
	userCache.Delete(id)
	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		if value.Value().CreatorID == id {
			CompareAndCloseRoom(value)
		}
		return true
	})
	return nil
}

func CompareAndCloseUser(user *UserEntry) error {
	if !userCache.CompareAndDelete(user.Value().ID, user) {
		return nil
	}
	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		if value.Value().CreatorID == user.Value().ID {
			CompareAndCloseRoom(value)
		}
		return true
	})
	return nil
}

func GetUserName(userID string) string {
	u, err := LoadOrInitUserByID(userID)
	if err != nil {
		return ""
	}
	return u.Value().Username
}
