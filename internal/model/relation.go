package model

import (
	"errors"
	"time"
)

type RoomUserStatus uint64

const (
	RoomUserStatusBanned RoomUserStatus = iota + 1
	RoomUserStatusPending
	RoomUserStatusActive
)

func (r RoomUserStatus) String() string {
	switch r {
	case RoomUserStatusBanned:
		return "banned"
	case RoomUserStatusPending:
		return "pending"
	case RoomUserStatusActive:
		return "active"
	default:
		return "unknown"
	}
}

type RoomUserPermission uint64

const (
	PermissionAll      RoomUserPermission = 0xffffffff
	PermissionEditRoom RoomUserPermission = 1 << iota
	PermissionEditUser
	PermissionCreateMovie
	PermissionEditCurrent
	PermissionSendChat
)

const (
	DefaultPermissions = PermissionCreateMovie | PermissionEditCurrent | PermissionSendChat
)

func (p RoomUserPermission) Has(permission RoomUserPermission) bool {
	return p&permission == permission
}

type RoomUserRelation struct {
	CreatedAt   time.Time
	UpdatedAt   time.Time
	UserID      string         `gorm:"primarykey;type:char(32)"`
	RoomID      string         `gorm:"primarykey;type:char(32)"`
	Status      RoomUserStatus `gorm:"not null;default:2"`
	Permissions RoomUserPermission
}

var ErrNoPermission = errors.New("no permission")

func (r *RoomUserRelation) HasPermission(permission RoomUserPermission) bool {
	switch r.Status {
	case RoomUserStatusActive:
		return r.Permissions.Has(permission)
	default:
		return false
	}
}
