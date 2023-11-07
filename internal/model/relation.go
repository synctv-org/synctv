package model

import (
	"errors"
	"time"
)

type RoomUserStatus uint

const (
	RoomRoleBanned RoomUserStatus = iota + 1
	RoomRolePending
	RoomRoleActive
)

func (r RoomUserStatus) String() string {
	switch r {
	case RoomRoleBanned:
		return "banned"
	case RoomRolePending:
		return "pending"
	case RoomRoleActive:
		return "active"
	default:
		return "unknown"
	}
}

type RoomUserPermission uint32

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
	UserID      string         `gorm:"not null;primarykey"`
	RoomID      string         `gorm:"not null;primarykey"`
	Status      RoomUserStatus `gorm:"not null;default:2"`
	Permissions RoomUserPermission
}

var ErrNoPermission = errors.New("no permission")

func (r *RoomUserRelation) HasPermission(permission RoomUserPermission) bool {
	switch r.Status {
	case RoomRoleActive:
		return r.Permissions.Has(permission)
	default:
		return false
	}
}
