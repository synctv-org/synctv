package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
)

type CreateRoomConfig func(r *model.Room)

func WithSetting(setting model.Settings) CreateRoomConfig {
	return func(r *model.Room) {
		r.Settings = setting
	}
}

func WithCreator(creator *model.User) CreateRoomConfig {
	return func(r *model.Room) {
		r.CreatorID = creator.ID
		r.GroupUserRelations = []model.RoomUserRelation{
			{
				UserID:      creator.ID,
				Role:        model.RoomRoleCreator,
				Permissions: model.AllPermissions,
			},
		}
	}
}

func WithRelations(relations []model.RoomUserRelation) CreateRoomConfig {
	return func(r *model.Room) {
		r.GroupUserRelations = append(r.GroupUserRelations, relations...)
	}
}

func WithStatus(status model.RoomStatus) CreateRoomConfig {
	return func(r *model.Room) {
		r.Status = status
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
	err := db.Create(r).Error
	if err != nil && errors.Is(err, gorm.ErrDuplicatedKey) {
		return r, errors.New("room already exists")
	}
	return r, err
}

func GetRoomByID(id string) (*model.Room, error) {
	if len(id) != 36 {
		return nil, errors.New("room id is not 32 bit")
	}
	r := &model.Room{}
	err := db.Where("id = ?", id).First(r).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return r, errors.New("room not found")
	}
	return r, err
}

func GetRoomAndCreatorByID(id string) (*model.Room, error) {
	r := &model.Room{}
	err := db.Preload("Creator").Where("id = ?", id).First(r).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return r, errors.New("room not found")
	}
	return r, err
}

func ChangeRoomSetting(roomID string, setting model.Settings) error {
	err := db.Model(&model.Room{}).Where("id = ?", roomID).Update("setting", setting).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room not found")
	}
	return err
}

func ChangeUserPermission(roomID, userID string, permission model.Permission) error {
	err := db.Model(&model.RoomUserRelation{}).Where("room_id = ? AND user_id = ?", roomID, userID).Update("permissions", permission).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or user not found")
	}
	return err
}

func HasPermission(roomID, userID string, permission model.Permission) (bool, error) {
	ur := &model.RoomUserRelation{}
	err := db.Where("room_id = ? AND user_id = ?", roomID, userID).First(ur).Error
	if err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			err = errors.New("room or user not found")
		}
		return false, err
	}
	return ur.Permissions.Has(permission), nil
}

func DeleteRoomByID(roomID string) error {
	err := db.Unscoped().Where("id = ?", roomID).Delete(&model.Room{}).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room not found")
	}
	return err
}

func HasRoom(roomID string) (bool, error) {
	r := &model.Room{}
	err := db.Where("id = ?", roomID).First(r).Error
	if err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			err = nil
		}
		return false, err
	}
	return true, nil
}

func HasRoomByName(name string) (bool, error) {
	r := &model.Room{}
	err := db.Where("name = ?", name).First(r).Error
	if err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			err = nil
		}
		return false, err
	}
	return true, nil
}

func SetRoomPassword(roomID, password string) error {
	var hashedPassword []byte
	if password != "" {
		var err error
		hashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
	}
	return SetRoomHashedPassword(roomID, hashedPassword)
}

func SetRoomHashedPassword(roomID string, hashedPassword []byte) error {
	err := db.Model(&model.Room{}).Where("id = ?", roomID).Update("hashed_password", hashedPassword).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room not found")
	}
	return err
}

func GetAllRooms(scopes ...func(*gorm.DB) *gorm.DB) []*model.Room {
	rooms := []*model.Room{}
	db.Scopes(scopes...).Find(&rooms)
	return rooms
}

func GetAllRoomsWithoutHidden(scopes ...func(*gorm.DB) *gorm.DB) []*model.Room {
	rooms := []*model.Room{}
	db.Where("settings_hidden = ?", false).Scopes(scopes...).Find(&rooms)
	return rooms
}

func GetAllRoomsWithoutHiddenCount(scopes ...func(*gorm.DB) *gorm.DB) int64 {
	var count int64
	db.Model(&model.Room{}).Where("settings_hidden = ?", false).Scopes(scopes...).Count(&count)
	return count
}

func GetAllRoomsAndCreator(scopes ...func(*gorm.DB) *gorm.DB) []*model.Room {
	rooms := []*model.Room{}
	db.Preload("Creator").Scopes(scopes...).Find(&rooms)
	return rooms
}

func GetAllRoomsByUserID(userID string) []*model.Room {
	rooms := []*model.Room{}
	db.Where("creator_id = ?", userID).Find(&rooms)
	return rooms
}
