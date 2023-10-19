package op

import (
	"hash/crc32"
	"time"

	"github.com/bluele/gcache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
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
		User:    *u,
		version: crc32.ChecksumIEEE(u.HashedPassword),
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
		User:    *u,
		version: crc32.ChecksumIEEE(u.HashedPassword),
	}

	return u2, userCache.SetWithExpire(u.ID, u2, time.Hour)
}

var ErrInvalidUsernameOrPassword = bcrypt.ErrMismatchedHashAndPassword

func CreateUser(username, password string, conf ...db.CreateUserConfig) (*User, error) {
	if username == "" || password == "" {
		return nil, ErrInvalidUsernameOrPassword
	}
	hashedPassword, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
	if err != nil {
		return nil, err
	}
	u, err := db.CreateUser(username, hashedPassword, conf...)
	if err != nil {
		return nil, err
	}

	u2 := &User{
		User:    *u,
		version: crc32.ChecksumIEEE(u.HashedPassword),
	}

	return u2, userCache.SetWithExpire(u.ID, u2, time.Hour)
}

func SetUserPassword(userID uint, password string) error {
	u, err := GetUserById(userID)
	if err != nil {
		return err
	}
	return u.SetPassword(password)
}

func DeleteUserByID(userID uint) error {
	rs, err := db.GetAllRoomsByUserID(userID)
	if err != nil {
		return err
	}
	err = db.DeleteUserByID(userID)
	if err != nil {
		return err
	}
	userCache.Remove(userID)

	for _, r := range rs {
		r2, loaded := roomCache.LoadAndDelete(r.ID)
		if loaded {
			r2.close()
		}
	}
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
