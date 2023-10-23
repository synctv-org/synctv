package model

import (
	"time"

	"github.com/synctv-org/synctv/internal/provider"
)

type UserProvider struct {
	Provider       provider.OAuth2Provider `gorm:"primarykey"`
	ProviderUserID uint                    `gorm:"primarykey;autoIncrement:false"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         uint `gorm:"not null"`
}
