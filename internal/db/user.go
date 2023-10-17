package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm/clause"
)

func CreateUser(username string, hashedPassword []byte) (*model.User, error) {
	u := &model.User{
		Username:       username,
		HashedPassword: hashedPassword,
	}
	return u, db.Where(*u).FirstOrCreate(u).Error
}

func AddUserToRoom(userID uint, roomID uint, role model.Role, permission model.Permission) error {
	ur := &model.RoomUserRelation{
		UserID:      userID,
		RoomID:      roomID,
		Role:        role,
		Permissions: permission,
	}
	return db.Attrs(ur).FirstOrCreate(ur).Error
}

func GetUserByUsername(username string) (*model.User, error) {
	u := &model.User{}
	err := db.Where("username = ?", username).First(u).Error
	return u, err
}

func GetUserByID(id uint) (*model.User, error) {
	u := &model.User{}
	err := db.Where("id = ?", id).First(u).Error
	return u, err
}

func GetUsersByRoomID(roomID uint) ([]model.User, error) {
	users := []model.User{}
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ?", roomID).Find(&users).Error
	return users, err
}

func DeleteUserByID(userID uint) error {
	return db.Where("id = ?", userID).Delete(&model.User{}).Error
}

func LoadAndDeleteUserByID(userID uint, columns ...clause.Column) (*model.User, error) {
	u := &model.User{}
	err := db.Clauses(clause.Returning{Columns: columns}).Where("id = ?", userID).Delete(u).Error
	return u, err
}

func DeleteUserByUsername(username string) error {
	return db.Where("username = ?", username).Delete(&model.User{}).Error
}

func LoadAndDeleteUserByUsername(username string, columns ...clause.Column) (*model.User, error) {
	u := &model.User{}
	err := db.Clauses(clause.Returning{Columns: columns}).Where("username = ?", username).Delete(u).Error
	return u, err
}

func SetUserPassword(userID uint, hashedPassword []byte) error {
	return db.Model(&model.User{}).Where("id = ?", userID).Update("hashed_password", hashedPassword).Error
}

func UpdateUser(u *model.User) error {
	return db.Save(u).Error
}
