package model

import (
	"fmt"
	"math/rand"
	"time"

	"github.com/google/uuid"
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
	ID                   string `gorm:"primaryKey;type:varchar(36)" json:"id"`
	CreatedAt            time.Time
	UpdatedAt            time.Time
	Providers            []UserProvider        `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Username             string                `gorm:"not null;uniqueIndex"`
	Role                 Role                  `gorm:"not null;default:pending"`
	GroupUserRelations   []RoomUserRelation    `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Rooms                []Room                `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies               []Movie               `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:SET NULL"`
	StreamingVendorInfos []StreamingVendorInfo `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
}

func (u *User) BeforeCreate(tx *gorm.DB) error {
	var existingUser User
	err := tx.Where("username = ?", u.Username).First(&existingUser).Error
	if err == nil {
		u.Username = fmt.Sprintf("%s#%d", u.Username, rand.Intn(9999))
	}
	if u.ID == "" {
		u.ID = uuid.NewString()
	}
	return nil
}

func (u *User) IsRoot() bool {
	return u.Role == RoleRoot
}

func (u *User) IsAdmin() bool {
	return u.Role == RoleAdmin || u.IsRoot()
}

func (u *User) IsPending() bool {
	return u.Role == RolePending
}

func (u *User) IsBanned() bool {
	return u.Role == RoleBanned
}
