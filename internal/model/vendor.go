package model

import (
	"net/http"
	"time"
)

type StreamingVendor string

const (
	StreamingVendorBilibili StreamingVendor = "bilibili"
)

type StreamingVendorInfo struct {
	UserID    string          `gorm:"not null;primarykey"`
	Vendor    StreamingVendor `gorm:"not null;primarykey"`
	CreatedAt time.Time
	UpdatedAt time.Time
	VendorToken
}

type VendorToken struct {
	Cookies []*http.Cookie `gorm:"serializer:fastjson"`
}
