package model

import (
	"github.com/synctv-org/synctv/internal/provider"
	"gorm.io/gorm"
)

type UserProvider struct {
	gorm.Model
	UserID         uint                    `gorm:"not null"`
	Provider       provider.OAuth2Provider `gorm:"not null;uniqueIndex:provider_user_id"`
	ProviderUserID uint                    `gorm:"not null;uniqueIndex:provider_user_id"`
}
