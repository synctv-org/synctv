package model

import (
	"fmt"
	"math/rand"

	"gorm.io/gorm"
)

type Role uint8

const (
	RoleBanned Role = iota
	RoleUser
	RoleAdmin
	RoleRoot
)

type User struct {
	gorm.Model
	Providers          []UserProvider     `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Username           string             `gorm:"not null;uniqueIndex"`
	Role               Role               `gorm:"not null"`
	GroupUserRelations []RoomUserRelation `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Rooms              []Room             `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies             []Movie            `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:SET NULL"`
}

func (u *User) BeforeCreate(tx *gorm.DB) error {
	var existingUser User
	err := tx.Where("username = ?", u.Username).First(&existingUser).Error
	if err == nil {
		u.Username = fmt.Sprintf("%s#%d", u.Username, rand.Intn(9999))
	}
	return nil
}
