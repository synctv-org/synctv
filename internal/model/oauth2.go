package model

import (
	"time"

	"github.com/synctv-org/synctv/internal/provider"
)

type UserProvider struct {
	Provider       provider.OAuth2Provider `gorm:"not null;primarykey;uniqueIndex:idx_provider_user_id"`
	ProviderUserID string                  `gorm:"not null;primarykey"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         string `gorm:"not null;uniqueIndex:idx_provider_user_id"`
}
