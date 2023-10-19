package model

import (
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type Room struct {
	gorm.Model
	Name string `gorm:"not null;uniqueIndex"`
	Setting
	CreatorID          uint `gorm:"index"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
	Movies             []Movie            `gorm:"foreignKey:RoomID;constraint:OnUpdate:CASCADE,OnDelete:CASCADE"`
}

func (r *Room) CheckPassword(password string) bool {
	return len(r.HashedPassword) == 0 || bcrypt.CompareHashAndPassword(r.HashedPassword, stream.StringToBytes(password)) == nil
}
