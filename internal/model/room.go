package model

import (
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type RoomStatus uint

const (
	RoomStatusBanned  RoomStatus = 1
	RoomStatusPending RoomStatus = 2
	RoomStatusActive  RoomStatus = 3
)

func (r RoomStatus) String() string {
	switch r {
	case RoomStatusBanned:
		return "banned"
	case RoomStatusPending:
		return "pending"
	case RoomStatusActive:
		return "active"
	default:
		return "unknown"
	}
}

type Room struct {
	ID                 string `gorm:"not null;primaryKey;type:varchar(32)" json:"id"`
	CreatedAt          time.Time
	UpdatedAt          time.Time
	Status             RoomStatus   `gorm:"not null;default:2"`
	Name               string       `gorm:"not null;uniqueIndex"`
	Settings           RoomSettings `gorm:"embedded;embeddedPrefix:settings_"`
	CreatorID          string       `gorm:"index"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies             []Movie            `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
}

func (r *Room) BeforeCreate(tx *gorm.DB) error {
	if r.ID == "" {
		r.ID = utils.SortUUID()
	}
	return nil
}

type RoomSettings struct {
	Hidden                 bool               `json:"hidden"`
	CanCreateMovie         bool               `gorm:"default:true" json:"canCreateMovie"`
	CanEditCurrent         bool               `gorm:"default:true" json:"canEditCurrent"`
	CanSendChat            bool               `gorm:"default:true" json:"canSendChat"`
	DisableJoinNewUser     bool               `gorm:"default:false" json:"disableJoinNewUser"`
	JoinNeedReview         bool               `gorm:"default:false" json:"joinNeedReview"`
	UserDefaultPermissions RoomUserPermission `json:"userDefaultPermissions"`
}

func (r *Room) NeedPassword() bool {
	return len(r.HashedPassword) != 0
}

func (r *Room) CheckPassword(password string) bool {
	return !r.NeedPassword() || bcrypt.CompareHashAndPassword(r.HashedPassword, stream.StringToBytes(password)) == nil
}

func (r *Room) IsBanned() bool {
	return r.Status == RoomStatusBanned
}

func (r *Room) IsPending() bool {
	return r.Status == RoomStatusPending
}

func (r *Room) IsActive() bool {
	return r.Status == RoomStatusActive
}
