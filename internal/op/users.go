package op

import (
	"errors"
	"time"

	"github.com/bluele/gcache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/gencontainer/synccache"
)

var userCache gcache.Cache

var (
	ErrUserBanned  = errors.New("user banned")
	ErrUserPending = errors.New("user pending, please wait for admin to approve")
)

func GetUserById(id string) (*User, error) {
	i, err := userCache.Get(id)
	if err == nil {
		return i.(*User), nil
	}

	u, err := db.GetUserByID(id)
	if err != nil {
		return nil, err
	}

	switch u.Role {
	case model.RoleBanned:
		return nil, ErrUserBanned
	case model.RolePending:
		return nil, ErrUserPending
	}

	u2 := &User{
		User: *u,
	}

	return u2, userCache.SetWithExpire(id, u2, time.Hour)
}

func CreateUser(username string, p provider.OAuth2Provider, pid string, conf ...db.CreateUserConfig) (*User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateUser(username, p, pid, conf...)
	if err != nil {
		return nil, err
	}

	u2 := &User{
		User: *u,
	}

	return u2, userCache.SetWithExpire(u.ID, u2, time.Hour)
}

func CreateOrLoadUser(username string, p provider.OAuth2Provider, pid string, conf ...db.CreateUserConfig) (*User, error) {
	if username == "" {
		return nil, errors.New("username cannot be empty")
	}
	u, err := db.CreateOrLoadUser(username, p, pid, conf...)
	if err != nil {
		return nil, err
	}

	u2 := &User{
		User: *u,
	}

	return u2, userCache.SetWithExpire(u.ID, u2, time.Hour)
}

func GetUserByProvider(p provider.OAuth2Provider, pid string) (*User, error) {
	uid, err := db.GetProviderUserID(p, pid)
	if err != nil {
		return nil, err
	}

	return GetUserById(uid)
}

func DeleteUserByID(userID string) error {
	err := db.DeleteUserByID(userID)
	if err != nil {
		return err
	}
	userCache.Remove(userID)

	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		v := value.Value()
		if v.CreatorID == userID {
			roomCache.CompareAndDelete(key, value)
		}
		return true
	})

	return nil
}

func SaveUser(u *model.User) error {
	defer userCache.Remove(u.ID)
	return db.SaveUser(u)
}

func GetUserName(userID string) string {
	u, err := GetUserById(userID)
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
	userCache.Remove(userID)

	err = db.SetRoomStatusByCreator(userID, model.RoomStatusBanned)
	if err != nil {
		return err
	}

	switch role {
	case model.RoleBanned:
		roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
			v := value.Value()
			if v.CreatorID == userID {
				if roomCache.CompareAndDelete(key, value) {
					v.close()
				}
			}
			return true
		})
	}

	return nil
}
