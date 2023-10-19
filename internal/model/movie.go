package model

import "gorm.io/gorm"

type Movie struct {
	gorm.Model
	Position uint `gorm:"not null"`
	RoomID   uint `gorm:"not null;index"`
	Room     Room `gorm:"foreignKey:RoomID"`
	MovieInfo
}

type MovieInfo struct {
	BaseMovieInfo
	PullKey   string `json:"pullKey"`
	CreatorID uint   `gorm:"not null;index" json:"creatorId"`
}

type BaseMovieInfo struct {
	Url        string            `json:"url"`
	Name       string            `gorm:"not null" json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `json:"type"`
	Headers    map[string]string `gorm:"serializer:fastjson" json:"headers"`
}
