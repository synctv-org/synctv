package model

import (
	"gorm.io/gorm"
)

type Movie struct {
	gorm.Model
	Position uint `gorm:"not null"`
	RoomID   uint `gorm:"not null"`
	MovieInfo
}

type MovieInfo struct {
	BaseMovieInfo
	PullKey   string
	CreatorID uint `gorm:"not null"`
}

type BaseMovieInfo struct {
	Url        string `gorm:"varchar(4096)"`
	Name       string `gorm:"not null;varchar(256)"`
	Live       bool
	Proxy      bool
	RtmpSource bool
	Type       string
	Headers    map[string]string `gorm:"serializer:json"`
}
