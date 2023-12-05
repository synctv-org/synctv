package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

type CreateRoomUserRelationConfig func(r *model.RoomUserRelation)

func WithRoomUserRelationStatus(status model.RoomUserStatus) CreateRoomUserRelationConfig {
	return func(r *model.RoomUserRelation) {
		r.Status = status
	}
}

func WithRoomUserRelationPermissions(permissions model.RoomUserPermission) CreateRoomUserRelationConfig {
	return func(r *model.RoomUserRelation) {
		r.Permissions = permissions
	}
}

func FirstOrCreateRoomUserRelation(roomID, userID string, conf ...CreateRoomUserRelationConfig) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{}
	d := &model.RoomUserRelation{
		RoomID:      roomID,
		UserID:      userID,
		Permissions: model.DefaultPermissions,
	}
	for _, c := range conf {
		c(d)
	}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).Attrs(d).FirstOrCreate(roomUserRelation).Error
	return roomUserRelation, err
}

func GetRoomUserRelation(roomID, userID string) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(roomUserRelation).Error
	return roomUserRelation, HandleNotFound(err, "room or user")
}

func SetRoomUserStatus(roomID string, userID string, status model.RoomUserStatus) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("status", status).Error
	return HandleNotFound(err, "room or user")
}

func SetUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
	return HandleNotFound(err, "room or user")
}

func AddUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions | ?", permission)).Error
	return HandleNotFound(err, "room or user")
}

func RemoveUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions & ?", ^permission)).Error
	return HandleNotFound(err, "room or user")
}

func GetAllRoomUsersRelation(roomID string, scopes ...func(*gorm.DB) *gorm.DB) []*model.RoomUserRelation {
	var roomUserRelations []*model.RoomUserRelation
	db.Where("room_id = ?", roomID).Scopes(scopes...).Find(&roomUserRelations)
	return roomUserRelations
}

func GetAllRoomUsersRelationCount(roomID string, scopes ...func(*gorm.DB) *gorm.DB) int64 {
	var count int64
	db.Model(&model.RoomUserRelation{}).Where("room_id = ?", roomID).Scopes(scopes...).Count(&count)
	return count
}
