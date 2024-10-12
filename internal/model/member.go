package model

import (
	"errors"
	"math"
	"time"
)

type RoomMemberStatus uint8

const (
	RoomMemberStatusNotJoined RoomMemberStatus = iota
	RoomMemberStatusBanned
	RoomMemberStatusPending
	RoomMemberStatusActive
)

func (r RoomMemberStatus) String() string {
	switch r {
	case RoomMemberStatusBanned:
		return "banned"
	case RoomMemberStatusPending:
		return "pending"
	case RoomMemberStatusActive:
		return "active"
	default:
		return "unknown"
	}
}

func (r RoomMemberStatus) IsPending() bool {
	return r == RoomMemberStatusPending
}

func (r RoomMemberStatus) IsActive() bool {
	return r == RoomMemberStatusActive
}

func (r RoomMemberStatus) IsNotActive() bool {
	return r != RoomMemberStatusActive
}

func (r RoomMemberStatus) IsBanned() bool {
	return r == RoomMemberStatusBanned
}

type RoomMemberPermission uint32

const (
	PermissionGetMovieList RoomMemberPermission = 1 << iota
	PermissionAddMovie
	PermissionDeleteMovie
	PermissionEditMovie
	PermissionSetCurrentMovie
	PermissionSetCurrentStatus
	PermissionSendChatMessage

	AllPermissions     RoomMemberPermission = math.MaxUint32
	NoPermission       RoomMemberPermission = 0
	DefaultPermissions RoomMemberPermission = PermissionGetMovieList | PermissionSendChatMessage
)

func (p RoomMemberPermission) Has(permission RoomMemberPermission) bool {
	return p&permission == permission
}

func (p RoomMemberPermission) Add(permission RoomMemberPermission) RoomMemberPermission {
	return p | permission
}

func (p RoomMemberPermission) Remove(permission RoomMemberPermission) RoomMemberPermission {
	return p &^ permission
}

type RoomMemberRole uint8

const (
	RoomMemberRoleUnknown RoomMemberRole = iota
	RoomMemberRoleMember
	RoomMemberRoleAdmin
	RoomMemberRoleCreator
)

func (r RoomMemberRole) String() string {
	switch r {
	case RoomMemberRoleMember:
		return "member"
	case RoomMemberRoleAdmin:
		return "admin"
	case RoomMemberRoleCreator:
		return "creator"
	default:
		return "unknown"
	}
}

func (r RoomMemberRole) IsCreator() bool {
	return r == RoomMemberRoleCreator
}

func (r RoomMemberRole) IsAdmin() bool {
	return r == RoomMemberRoleAdmin || r.IsCreator()
}

func (r RoomMemberRole) IsMember() bool {
	return r == RoomMemberRoleMember || r.IsAdmin()
}

type RoomAdminPermission uint32

const (
	PermissionApprovePendingMember RoomAdminPermission = 1 << iota
	PermissionBanRoomMember
	PermissionSetUserPermission
	PermissionSetRoomSettings
	PermissionSetRoomPassword
	PermissionDeleteRoom

	AllAdminPermissions     RoomAdminPermission = math.MaxUint32
	NoAdminPermission       RoomAdminPermission = 0
	DefaultAdminPermissions RoomAdminPermission = PermissionApprovePendingMember |
		PermissionBanRoomMember |
		PermissionSetUserPermission |
		PermissionSetRoomSettings |
		PermissionSetRoomPassword
)

func (p RoomAdminPermission) Has(permission RoomAdminPermission) bool {
	return p&permission == permission
}

func (p RoomAdminPermission) Add(permission RoomAdminPermission) RoomAdminPermission {
	return p | permission
}

func (p RoomAdminPermission) Remove(permission RoomAdminPermission) RoomAdminPermission {
	return p &^ permission
}

type RoomMember struct {
	CreatedAt        time.Time
	UpdatedAt        time.Time
	UserID           string           `gorm:"primarykey;type:char(32)"`
	RoomID           string           `gorm:"primarykey;type:char(32)"`
	Status           RoomMemberStatus `gorm:"not null;default:2"`
	Role             RoomMemberRole   `gorm:"not null;default:1"`
	Permissions      RoomMemberPermission
	AdminPermissions RoomAdminPermission
}

var ErrNoPermission = errors.New("no permission")

func (r *RoomMember) HasPermission(permission RoomMemberPermission) bool {
	if r.Role.IsAdmin() {
		return true
	}
	if !r.Role.IsMember() {
		return false
	}
	if r.Status != RoomMemberStatusActive {
		return false
	}
	return r.Permissions.Has(permission)
}

func (r *RoomMember) HasAdminPermission(permission RoomAdminPermission) bool {
	if r.Role.IsCreator() {
		return true
	}
	if !r.Role.IsAdmin() {
		return false
	}
	if r.Status != RoomMemberStatusActive {
		return false
	}
	return r.AdminPermissions.Has(permission)
}
