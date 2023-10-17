package model

import (
	"gorm.io/gorm"
)

type Movie struct {
	gorm.Model
	Position  uint `gorm:"not null" json:"-"`
	RoomID    uint `gorm:"not null" json:"roomId"`
	MovieInfo `gorm:"embedded"`
}

type MovieInfo struct {
	BaseMovieInfo `gorm:"embedded"`
	PullKey       string `gorm:"varchar(16)" json:"pullKey"`
	CreatorID     uint   `gorm:"not null" json:"creatorId"`
}

type BaseMovieInfo struct {
	Url        string            `gorm:"varchar(4096)" json:"url"`
	Name       string            `gorm:"not null;varchar(256)" json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `gorm:"varchar(32)" json:"type"`
	Headers    map[string]string `gorm:"serializer:json" json:"headers"`
}
