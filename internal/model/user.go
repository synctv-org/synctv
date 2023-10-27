package model

import (
	"fmt"
	"math/rand"
	"time"

	"gorm.io/gorm"
)

type Role string

const (
	RoleBanned  Role = "banned"
	RolePending Role = "pending"
	RoleUser    Role = "user"
	RoleAdmin   Role = "admin"
	RoleRoot    Role = "root"
)

type User struct {
	ID                 uint `gorm:"primarykey"`
	CreatedAt          time.Time
	UpdatedAt          time.Time
	Providers          []UserProvider     `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Username           string             `gorm:"not null;uniqueIndex"`
	Role               Role               `gorm:"not null;default:user"`
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

func (u *User) IsRoot() bool {
	return u.Role == RoleRoot
}

func (u *User) IsAdmin() bool {
	return u.Role == RoleAdmin
}

func (u *User) IsBanned() bool {
	return u.Role == RoleBanned
}
