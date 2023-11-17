package model

import (
	"time"

	"github.com/synctv-org/synctv/internal/provider"
)

type UserProvider struct {
	Provider       provider.OAuth2Provider `gorm:"not null;primarykey"`
	ProviderUserID string                  `gorm:"not null;primarykey;autoIncrement:false"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         string `gorm:"not null;index"`
}
