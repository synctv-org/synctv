package model

import (
	"time"

	"github.com/synctv-org/synctv/internal/provider"
)

type UserProvider struct {
	ID             uint `gorm:"primarykey"`
	CreatedAt      time.Time
	UpdatedAt      time.Time
	UserID         uint                    `gorm:"not null"`
	Provider       provider.OAuth2Provider `gorm:"not null;uniqueIndex:provider_user_id"`
	ProviderUserID uint                    `gorm:"not null;uniqueIndex:provider_user_id"`
}
