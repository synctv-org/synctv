package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func GetRoomUserRelation(roomID, userID uint) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).Attrs(&model.RoomUserRelation{
		RoomID:      roomID,
		UserID:      userID,
		Role:        model.RoomRoleUser,
		Permissions: model.DefaultPermissions,
	}).FirstOrInit(roomUserRelation).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return roomUserRelation, errors.New("room or user not found")
	}
	return roomUserRelation, err
}

func CreateRoomUserRelation(roomID, userID uint, role model.Role, permissions model.Permission) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{
		RoomID:      roomID,
		UserID:      userID,
		Role:        role,
		Permissions: permissions,
	}
	err := db.Create(roomUserRelation).Error
	return roomUserRelation, err
}

func SetUserRole(roomID uint, userID uint, role model.Role) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("role", role).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func SetUserPermission(roomID uint, userID uint, permission model.Permission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func AddUserPermission(roomID uint, userID uint, permission model.Permission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions | ?", permission)).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func RemoveUserPermission(roomID uint, userID uint, permission model.Permission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions & ?", ^permission)).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func DeleteUserPermission(roomID uint, userID uint) error {
	err := db.Unscoped().Where("room_id = ? AND user_id = ?", roomID, userID).Delete(&model.RoomUserRelation{}).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}
