package model

import (
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type User struct {
	gorm.Model
	Username           string `gorm:"not null;uniqueIndex;varchar(32)"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation
}

func (u *User) CheckPassword(password string) bool {
	return bcrypt.CompareHashAndPassword(u.HashedPassword, stream.StringToBytes(password)) == nil
}

func (u *User) SetPassword(password string) error {
	hashedPassword, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}
	u.HashedPassword = hashedPassword
	return nil
}
