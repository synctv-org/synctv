package model

import (
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type Role uint8

const (
	RoleBanned Role = iota
	RoleUser
	RoleAdmin
)

type User struct {
	gorm.Model
	Username           string `gorm:"not null;uniqueIndex"`
	Role               Role   `gorm:"not null"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies             []Movie            `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:SET NULL"`
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
