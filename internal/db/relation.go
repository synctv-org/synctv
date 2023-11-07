package db

import (
	"errors"

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
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return roomUserRelation, errors.New("room or user not found")
	}
	return roomUserRelation, err
}

func SetRoomUserStatus(roomID string, userID string, status model.RoomUserStatus) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("status", status).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func SetUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func AddUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions | ?", permission)).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func RemoveUserPermission(roomID string, userID string, permission model.RoomUserPermission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions & ?", ^permission)).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}
