package model

import (
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type Room struct {
	gorm.Model
	Name string `gorm:"not null;uniqueIndex;varchar(32)"`
	Setting
	CreatorID          uint `gorm:"not null"`
	HashedPassword     []byte
	GroupUserRelations []RoomUserRelation
	Movies             []Movie
}

func (r *Room) CheckPassword(password string) bool {
	return len(r.HashedPassword) == 0 || bcrypt.CompareHashAndPassword(r.HashedPassword, stream.StringToBytes(password)) == nil
}
