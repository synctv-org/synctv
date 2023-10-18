package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
)

func GetRoomUserRelation(roomID, userID uint) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(roomUserRelation).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return roomUserRelation, errors.New("room or user not found")
	}
	return roomUserRelation, err
}
