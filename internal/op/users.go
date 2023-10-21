package op

import (
	"errors"
	"time"

	"github.com/bluele/gcache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
)

var userCache gcache.Cache

func GetUserById(id uint) (*User, error) {
	i, err := userCache.Get(id)
	if err == nil {
		return i.(*User), nil
	}

	u, err := db.GetUserByID(id)
	if err != nil {
		return nil, err
	}

	u2 := &User{
		User: *u,
	}

	return u2, userCache.SetWithExpire(id, u2, time.Hour)
}

// slow
func GetUserByUsername(username string) (*User, error) {
	u, err := db.GetUserByUsername(username)
	if err != nil {
		return nil, err
	}

	u2 := &User{
		User: *u,
	}

	return u2, userCache.SetWithExpire(u.ID, u2, time.Hour)
}

func CreateUser(username string, p provider.OAuth2Provider, pid uint, conf ...db.CreateUserConfig) (*User, error) {
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

func CreateOrLoadUser(username string, p provider.OAuth2Provider, pid uint, conf ...db.CreateUserConfig) (*User, error) {
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

func DeleteUserByID(userID uint) error {
	err := db.DeleteUserByID(userID)
	if err != nil {
		return err
	}
	userCache.Remove(userID)

	roomCache.Range(func(key uint, value *Room) bool {
		if value.CreatorID == userID {
			roomCache.Delete(key)
			value.close()
		}
		return true
	})

	return nil
}

func SaveUser(u *model.User) error {
	userCache.Remove(u.ID)
	return db.SaveUser(u)
}

func GetUserName(userID uint) string {
	u, err := GetUserById(userID)
	if err != nil {
		return ""
	}
	return u.Username
}
