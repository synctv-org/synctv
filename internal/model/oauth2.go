package model

import (
	"time"
)

type UserProvider struct {
	Provider       string `gorm:"primarykey;type:varchar(32);uniqueIndex:idx_provider_user_id"`
	ProviderUserID string `gorm:"primarykey;type:varchar(64)"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         string `gorm:"not null;type:char(32);uniqueIndex:idx_provider_user_id"`
}
