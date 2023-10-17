package db

import "github.com/synctv-org/synctv/internal/model"

func GetRoomUserRelation(roomID, userID uint) (*model.RoomUserRelation, error) {
	roomUserRelation := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(roomUserRelation).Error
	return roomUserRelation, err
}
