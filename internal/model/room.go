package model

import (
	"time"

	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type RoomStatus string

const (
	RoomStatusBanned  RoomStatus = "banned"
	RoomStatusPending RoomStatus = "pending"
	RoomStatusStopped RoomStatus = "stopped"
	RoomStatusActive  RoomStatus = "active"
)

type Room struct {
	ID                 string `gorm:"not null;primaryKey;type:varchar(36)" json:"id"`
	CreatedAt          time.Time
	UpdatedAt          time.Time
	Status             RoomStatus `gorm:"not null;default:pending"`
	Name               string     `gorm:"not null;uniqueIndex"`
	Settings           Settings   `gorm:"embedded;embeddedPrefix:settings_"`
	CreatorID          string     `gorm:"index"`
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

type Settings struct {
	Hidden bool `json:"hidden"`
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

func (r *Room) IsStopped() bool {
	return r.Status == RoomStatusStopped
}

func (r *Room) IsActive() bool {
	return r.Status == RoomStatusActive
}
