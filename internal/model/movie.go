package model

import "gorm.io/gorm"

type Movie struct {
	gorm.Model
	Position  uint `gorm:"not null"`
	RoomID    uint `gorm:"not null;index"`
	Room      Room `gorm:"foreignKey:RoomID"`
	MovieInfo `gorm:"embedded"`
}

type MovieInfo struct {
	BaseMovieInfo `gorm:"embedded"`
	PullKey       string `gorm:"varchar(128)" json:"pullKey"`
	CreatorID     uint   `gorm:"not null;index" json:"creatorId"`
}

type BaseMovieInfo struct {
	Url        string            `gorm:"varchar(4096)" json:"url"`
	Name       string            `gorm:"not null;varchar(256)" json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `gorm:"varchar(32)" json:"type"`
	Headers    map[string]string `gorm:"serializer:fastjson" json:"headers"`
}
