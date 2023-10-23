package model

import "time"

type RoomRole uint32

const (
	RoomRoleBanned RoomRole = iota + 1
	RoomRoleUser
	RoomRoleCreator
)

type Permission uint32

const (
	CanRenameRoom Permission = 1 << iota
	CanSetAdmin
	CanSetRoomPassword
	CanSetRoomSetting
	CanSetUserPermission
	CanSetUserPassword
	CanCreateUserPublishKey
	CanEditUserMovies
	CanDeleteUserMovies
	CanCreateMovie
	CanChangeCurrentMovie
	CanChangeMovieStatus
	CanDeleteRoom
	AllPermissions Permission = 0xffffffff
)

const (
	DefaultPermissions = CanCreateMovie | CanChangeCurrentMovie | CanChangeMovieStatus
)

func (p Permission) Has(permission Permission) bool {
	return p&permission == permission
}

type RoomUserRelation struct {
	ID          uint `gorm:"primarykey"`
	CreatedAt   time.Time
	UpdatedAt   time.Time
	UserID      uint     `gorm:"not null;uniqueIndex:idx_user_room"`
	RoomID      uint     `gorm:"not null;uniqueIndex:idx_user_room"`
	Role        RoomRole `gorm:"not null"`
	Permissions Permission
}

func (r *RoomUserRelation) HasPermission(permission Permission) bool {
	switch r.Role {
	case RoomRoleCreator:
		return true
	case RoomRoleUser:
		return r.Permissions.Has(permission)
	default:
		return false
	}
}
