package model

import "gorm.io/gorm"

type Setting struct {
	gorm.Model
	RoomID uint `gorm:"uniqueIndex"`
	Hidden bool
}
