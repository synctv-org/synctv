package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

const (
	ErrRoomMemberNotFound = "room or member"
)

type CreateRoomMemberRelationConfig func(r *model.RoomMember)

func WithRoomMemberStatus(status model.RoomMemberStatus) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.Status = status
	}
}

func WithRoomMemberRole(role model.RoomMemberRole) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.Role = role
	}
}

func WithRoomMemberPermissions(
	permissions model.RoomMemberPermission,
) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.Permissions = permissions
	}
}

func WithRoomMemberAdminPermissions(
	permissions model.RoomAdminPermission,
) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.AdminPermissions = permissions
	}
}

func FirstOrCreateRoomMemberRelation(
	roomID, userID string,
	conf ...CreateRoomMemberRelationConfig,
) (*model.RoomMember, error) {
	roomMemberRelation := &model.RoomMember{}

	d := &model.RoomMember{
		RoomID:           roomID,
		UserID:           userID,
		Role:             model.RoomMemberRoleMember,
		Status:           model.RoomMemberStatusPending,
		Permissions:      model.NoPermission,
		AdminPermissions: model.NoAdminPermission,
	}
	for _, c := range conf {
		c(d)
	}

	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).
		Attrs(d).
		FirstOrCreate(roomMemberRelation).
		Error

	return roomMemberRelation, err
}

func GetRoomMember(roomID, userID string) (*model.RoomMember, error) {
	roomMemberRelation := &model.RoomMember{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(roomMemberRelation).Error
	return roomMemberRelation, HandleNotFound(err, ErrRoomMemberNotFound)
}

func RoomApprovePendingMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ? AND status = ?", roomID, userID, model.RoomMemberStatusPending).
		Update("status", model.RoomMemberStatusActive)

	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomBanMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("status", model.RoomMemberStatusBanned)
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomUnbanMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("status", model.RoomMemberStatusActive)
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func DeleteRoomMember(roomID, userID string) error {
	result := db.
		Where("NOT EXISTS (?)",
			db.Table("rooms").
				Select("1").
				Where("rooms.id = room_members.room_id AND rooms.creator_id = room_members.user_id"),
		).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Delete(&model.RoomMember{})

	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func SetMemberPermissions(roomID, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("permissions", permission)
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func AddMemberPermissions(roomID, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("permissions", db.Raw("permissions | ?", permission))
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RemoveMemberPermissions(roomID, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("permissions", db.Raw("permissions & ?", ^permission))
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

// func GetAllRoomMembersRelationCount(roomID string, scopes ...func(*gorm.DB) *gorm.DB) (int64,
// error) {
// 	var count int64
// 	err := db.Model(&model.RoomMember{}).Where("room_id = ?",
// roomID).Scopes(scopes...).Count(&count).Error
// 	return count, err
// }

func RoomSetAdminPermissions(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("admin_permissions", permissions)
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomAddAdminPermissions(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("admin_permissions", db.Raw("admin_permissions | ?", permissions))
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomRemoveAdminPermissions(
	roomID, userID string,
	permissions model.RoomAdminPermission,
) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("admin_permissions", db.Raw("admin_permissions & ?", ^permissions))
	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomSetAdmin(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Updates(map[string]any{
			"role":              model.RoomMemberRoleAdmin,
			"permissions":       model.AllPermissions,
			"admin_permissions": permissions,
		})

	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func RoomSetMember(roomID, userID string, permissions model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Updates(map[string]any{
			"role":              model.RoomMemberRoleMember,
			"permissions":       permissions,
			"admin_permissions": model.NoAdminPermission,
		})

	return HandleUpdateResult(result, ErrRoomMemberNotFound)
}

func GetRoomMembers(roomID string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.RoomMember, error) {
	var members []*model.RoomMember

	err := db.
		Where("room_id = ?", roomID).
		Scopes(scopes...).
		Find(&members).Error

	return members, err
}

func GetRoomMembersCount(roomID string, scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64

	err := db.
		Model(&model.RoomMember{}).
		Where("room_id = ?", roomID).
		Scopes(scopes...).
		Count(&count).Error

	return count, err
}
