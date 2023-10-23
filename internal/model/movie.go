package model

import "time"

type Movie struct {
	ID        uint      `gorm:"primarykey" json:"id"`
	CreatedAt time.Time `json:"-"`
	UpdatedAt time.Time `json:"-"`
	Position  uint      `gorm:"not null" json:"-"`
	RoomID    uint      `gorm:"not null;index" json:"-"`
	CreatorID uint      `gorm:"not null;index" json:"creatorId"`
	MovieInfo
}

type MovieInfo struct {
	Base    BaseMovieInfo `gorm:"embedded;embeddedPrefix:base_" json:"base"`
	PullKey string        `json:"pullKey"`
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
