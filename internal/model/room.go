package model

import (
	"time"

	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type Room struct {
	ID                 uint `gorm:"primarykey"`
	CreatedAt          time.Time
	UpdatedAt          time.Time
	Name               string   `gorm:"not null;uniqueIndex"`
	Settings           Settings `gorm:"embedded;embeddedPrefix:settings_"`
	CreatorID          uint     `gorm:"index"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies             []Movie            `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
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
