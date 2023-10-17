package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type CreateRoomConfig func(r *model.Room)

func WithSetting(setting model.Setting) CreateRoomConfig {
	return func(r *model.Room) {
		r.Setting = setting
	}
}

func WithCreaterID(createrID uint) CreateRoomConfig {
	return func(r *model.Room) {
		r.CreatorID = createrID
	}
}

func WithRelations(relations []model.RoomUserRelation) CreateRoomConfig {
	return func(r *model.Room) {
		r.GroupUserRelations = relations
	}
}

func CreateRoom(name, password string, conf ...CreateRoomConfig) (*model.Room, error) {
	var hashedPassword []byte
	if password != "" {
		var err error
		hashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return nil, err
		}
	}
	r := &model.Room{
		Name:           name,
		HashedPassword: hashedPassword,
	}
	for _, c := range conf {
		c(r)
	}
	return r, db.Create(r).Error
}

func GetRoomByName(name string) (*model.Room, error) {
	r := &model.Room{}
	err := db.Where("name = ?", name).First(r).Error
	return r, err
}

func GetRoomByID(id uint) (*model.Room, error) {
	r := &model.Room{}
	err := db.Where("id = ?", id).First(r).Error
	return r, err
}

func ChangeRoomSetting(roomID uint, setting model.Setting) error {
	return db.Model(&model.Room{}).Where("id = ?", roomID).Update("setting", setting).Error
}

func ChangeUserPermission(roomID uint, userID uint, permission model.Permission) error {
	return db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
}

func HasPermission(roomID uint, userID uint, permission model.Permission) (bool, error) {
	ur := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(ur).Error
	if err != nil {
		return false, err
	}
	return ur.Permissions.Has(permission), nil
}

func DeleteRoomByID(roomID uint) error {
	return db.Where("id = ?", roomID).Delete(&model.Room{}).Error
}

func HasRoom(roomID uint) (bool, error) {
	r := &model.Room{}
	err := db.Where("id = ?", roomID).First(r).Error
	if err != nil {
		return false, err
	}
	return true, nil
}

func HasRoomByName(name string) (bool, error) {
	r := &model.Room{}
	err := db.Where("name = ?", name).First(r).Error
	if err != nil {
		return false, err
	}
	return true, nil
}

func SetRoomPassword(roomID uint, password string) error {
	var hashedPassword []byte
	if password != "" {
		var err error
		hashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
	}
	return db.Model(&model.Room{}).Where("id = ?", roomID).Update("hashed_password", hashedPassword).Error
}

func SetRoomHashedPassword(roomID uint, hashedPassword []byte) error {
	return db.Model(&model.Room{}).Where("id = ?", roomID).Update("hashed_password", hashedPassword).Error
}

func SetUserRole(roomID uint, userID uint, role model.Role) error {
	return db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("role", role).Error
}

func SetUserPermission(roomID uint, userID uint, permission model.Permission) error {
	return db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
}

func AddUserPermission(roomID uint, userID uint, permission model.Permission) error {
	return db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions | ?", permission)).Error
}

func RemoveUserPermission(roomID uint, userID uint, permission model.Permission) error {
	return db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", db.Raw("permissions & ?", ^permission)).Error
}

func GetAllRooms() ([]model.Room, error) {
	rooms := []model.Room{}
	err := db.Find(&rooms).Error
	return rooms, err
}
