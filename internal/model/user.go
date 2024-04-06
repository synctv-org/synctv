package model

import (
	"fmt"
	"math/rand"
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type Role uint

const (
	RoleBanned  Role = 1
	RolePending Role = 2
	RoleUser    Role = 3
	RoleAdmin   Role = 4
	RoleRoot    Role = 5
)

func (r Role) String() string {
	switch r {
	case RoleBanned:
		return "banned"
	case RolePending:
		return "pending"
	case RoleUser:
		return "user"
	case RoleAdmin:
		return "admin"
	case RoleRoot:
		return "root"
	default:
		return "unknown"
	}
}

type User struct {
	ID                   string `gorm:"primaryKey;type:char(32)" json:"id"`
	CreatedAt            time.Time
	UpdatedAt            time.Time
	RegisteredByProvider bool                `gorm:"not null;default:false"`
	RegisteredByEmail    bool                `gorm:"not null;default:false"`
	UserProviders        []*UserProvider     `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Username             string              `gorm:"not null;uniqueIndex;type:varchar(32)"`
	HashedPassword       []byte              `gorm:"not null"`
	Email                string              `gorm:"type:varchar(128);uniqueIndex:,where:email <> ''"`
	Role                 Role                `gorm:"not null;default:2"`
	RoomUserRelations    []*RoomUserRelation `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Rooms                []Room              `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies               []Movie             `gorm:"foreignKey:CreatorID;constraint:OnUpdate:CASCADE,OnDelete:SET NULL"`
	BilibiliVendor       *BilibiliVendor     `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	AlistVendor          []*AlistVendor      `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	EmbyVendor           []*EmbyVendor       `gorm:"foreignKey:UserID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
}

func (u *User) CheckPassword(password string) bool {
	return bcrypt.CompareHashAndPassword(u.HashedPassword, stream.StringToBytes(password)) == nil
}

func (u *User) BeforeCreate(tx *gorm.DB) error {
	var existingUser User
	err := tx.Where("username = ?", u.Username).First(&existingUser).Error
	if err == nil {
		u.Username = fmt.Sprintf("%s#%d", u.Username, rand.Intn(9999))
	}
	if u.ID == "" {
		u.ID = utils.SortUUID()
	}
	return nil
}

func (u *User) IsRoot() bool {
	return u.Role == RoleRoot
}

func (u *User) IsAdmin() bool {
	return u.Role == RoleAdmin || u.IsRoot()
}

func (u *User) IsUser() bool {
	return u.Role == RoleUser || u.IsAdmin()
}

func (u *User) IsPending() bool {
	return u.Role == RolePending
}

func (u *User) IsBanned() bool {
	return u.Role == RoleBanned
}
