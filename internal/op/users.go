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

var (
	ErrUserBanned  = errors.New("user banned")
	ErrUserPending = errors.New("user pending, please wait for admin to approve")
)

func LoadOrInitUser(u *model.User) (*User, error) {
	switch u.Role {
	case model.RoleBanned:
		return nil, ErrUserBanned
	case model.RolePending:
		return nil, ErrUserPending
	}
	i, _ := userCache.LoadOrStore(u.ID, &User{
		User:    *u,
		version: crc32.ChecksumIEEE(u.HashedPassword),
	}, time.Hour)
	return i.Value(), nil
}

func LoadOrInitUserByID(id string) (*User, error) {
	u, ok := userCache.Load(id)
	if ok {
		u.SetExpiration(time.Now().Add(time.Hour))
		return u.Value(), nil
	}

	user, err := db.GetUserByID(id)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(user)
}

func LoadUserByUsername(username string) (*User, error) {
	u, err := db.GetUserByUsername(username)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CreateOrLoadUser(username string, password string, conf ...db.CreateUserConfig) (*User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateOrLoadUser(username, password, conf...)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CreateOrLoadUserWithProvider(username, password string, p provider.OAuth2Provider, pid string, conf ...db.CreateUserConfig) (*User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateOrLoadUserWithProvider(username, password, p, pid, conf...)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func GetUserByProvider(p provider.OAuth2Provider, pid string) (*User, error) {
	u, err := db.GetUserByProvider(p, pid)
	if err != nil {
		return nil, err
	}

	return LoadOrInitUser(u)
}

func CompareAndDeleteUser(user *User) error {
	err := db.DeleteUserByID(user.ID)
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
			CompareAndCloseRoomEntry(key, value)
		}
		return true
	})
	return nil
}

func CompareAndCloseUser(user *User) error {
	u, loaded := userCache.LoadAndDelete(user.ID)
	if loaded {
		if u.Value() != user {
			return errors.New("user compare failed")
		}
		if userCache.CompareAndDelete(user.ID, u) {
			roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
				if value.Value().CreatorID == user.ID {
					CompareAndCloseRoomEntry(key, value)
				}
				return true
			})
		}
	}
	return nil
}

func GetUserName(userID string) string {
	u, err := LoadOrInitUserByID(userID)
	if err != nil {
		return ""
	}
	return u.Username
}

func SetRoleByID(userID string, role model.Role) error {
	err := db.SetRoleByID(userID, role)
	if err != nil {
		return err
	}

	userCache.Delete(userID)

	switch role {
	case model.RoleBanned:
		err = db.SetRoomStatusByCreator(userID, model.RoomStatusBanned)
		if err != nil {
			return err
		}
		roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
			if value.Value().CreatorID == userID {
				CompareAndCloseRoomEntry(key, value)
			}
			return true
		})
	}

	return nil
}
