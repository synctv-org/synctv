package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
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

func WithRoomMemberPermissions(permissions model.RoomMemberPermission) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.Permissions = permissions
	}
}

func WithRoomMemberAdminPermissions(permissions model.RoomAdminPermission) CreateRoomMemberRelationConfig {
	return func(r *model.RoomMember) {
		r.AdminPermissions = permissions
	}
}

func FirstOrCreateRoomMemberRelation(roomID, userID string, conf ...CreateRoomMemberRelationConfig) (*model.RoomMember, error) {
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
	err := OnConflictDoNothing().Where("room_id = ? AND user_id = ?", roomID, userID).Attrs(d).FirstOrCreate(roomMemberRelation).Error
	return roomMemberRelation, err
}

func GetRoomMember(roomID, userID string) (*model.RoomMember, error) {
	roomMemberRelation := &model.RoomMember{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(roomMemberRelation).Error
	return roomMemberRelation, HandleNotFound(err, "room or user")
}

func RoomApprovePendingMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ? AND status = ?", roomID, userID, model.RoomMemberStatusPending).
		Update("status", model.RoomMemberStatusActive)

	return HandleUpdateResult(result, "room member")
}

func RoomBanMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).
		Where("room_id = ? AND user_id = ?", roomID, userID).
		Update("status", model.RoomMemberStatusBanned)
	return HandleUpdateResult(result, "room or user")
}

func RoomUnbanMember(roomID, userID string) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("status", model.RoomMemberStatusActive)
	return HandleUpdateResult(result, "room or user")
}

func DeleteRoomMember(roomID, userID string) error {
	result := db.Where("room_id = ? AND user_id = ?", roomID, userID).Delete(&model.RoomMember{})
	return HandleUpdateResult(result, "room or user")
}

func SetMemberPermissions(roomID string, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission)
	return HandleUpdateResult(result, "room or user")
}

func AddMemberPermissions(roomID string, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions | ?", permission))
	return HandleUpdateResult(result, "room or user")
}

func RemoveMemberPermissions(roomID string, userID string, permission model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions & ?", ^permission))
	return HandleUpdateResult(result, "room or user")
}

// func GetAllRoomMembersRelationCount(roomID string, scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
// 	var count int64
// 	err := db.Model(&model.RoomMember{}).Where("room_id = ?", roomID).Scopes(scopes...).Count(&count).Error
// 	return count, err
// }

func RoomSetAdminPermissions(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("admin_permissions", permissions)
	return HandleUpdateResult(result, "room or user")
}

func RoomAddAdminPermissions(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("admin_permissions", db.Raw("admin_permissions | ?", permissions))
	return HandleUpdateResult(result, "room or user")
}

func RoomRemoveAdminPermissions(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("admin_permissions", db.Raw("admin_permissions & ?", ^permissions))
	return HandleUpdateResult(result, "room or user")
}

func RoomSetAdmin(roomID, userID string, permissions model.RoomAdminPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Updates(map[string]interface{}{
		"role":              model.RoomMemberRoleAdmin,
		"permissions":       model.AllPermissions,
		"admin_permissions": permissions,
	})
	return HandleUpdateResult(result, "room or user")
}

func RoomSetMember(roomID, userID string, permissions model.RoomMemberPermission) error {
	result := db.Model(&model.RoomMember{}).Where("room_id = ? AND user_id = ?", roomID, userID).Updates(map[string]interface{}{
		"role":              model.RoomMemberRoleMember,
		"permissions":       permissions,
		"admin_permissions": model.NoAdminPermission,
	})
	return HandleUpdateResult(result, "room or user")
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
