package db

import (
	"errors"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

type CreateRoomConfig func(r *model.Room)

func WithSetting(setting *model.RoomSettings) CreateRoomConfig {
	return func(r *model.Room) {
		r.Settings = setting
	}
}

func WithCreator(creator *model.User) CreateRoomConfig {
	return func(r *model.Room) {
		r.CreatorID = creator.ID
		r.GroupUserRelations = []*model.RoomMember{
			{
				UserID:           creator.ID,
				Status:           model.RoomMemberStatusActive,
				Role:             model.RoomMemberRoleCreator,
				Permissions:      model.AllPermissions,
				AdminPermissions: model.AllAdminPermissions,
			},
		}
	}
}

func WithRelations(relations []*model.RoomMember) CreateRoomConfig {
	return func(r *model.Room) {
		r.GroupUserRelations = append(r.GroupUserRelations, relations...)
	}
}

func WithStatus(status model.RoomStatus) CreateRoomConfig {
	return func(r *model.Room) {
		r.Status = status
	}
}

func WithSettingHidden(hidden bool) CreateRoomConfig {
	return func(r *model.Room) {
		r.Settings.Hidden = hidden
	}
}

// if maxCount is 0, it will be ignored
func CreateRoom(name, password string, maxCount int64, conf ...CreateRoomConfig) (*model.Room, error) {
	r := &model.Room{
		Name:     name,
		Settings: model.DefaultRoomSettings(),
	}
	for _, c := range conf {
		c(r)
	}
	if password != "" {
		var err error
		r.HashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return nil, err
		}
	}

	return r, Transactional(func(tx *gorm.DB) error {
		if maxCount != 0 {
			var count int64
			tx.Model(&model.Room{}).Where("creator_id = ?", r.CreatorID).Count(&count)
			if count >= maxCount {
				return errors.New("room count is over limit")
			}
		}
		err := tx.Create(r).Error
		if err != nil {
			if errors.Is(err, gorm.ErrDuplicatedKey) {
				return errors.New("room already exists")
			}
			return err
		}
		return nil
	})
}

func GetRoomByID(id string) (*model.Room, error) {
	if len(id) != 32 {
		return nil, errors.New("room id is not 32 bit")
	}
	r := &model.Room{}
	err := db.
		Where("id = ?", id).
		Preload("Settings", "id = ?", id).
		First(r).Error
	return r, HandleNotFound(err, "room")
}

func SaveRoomSettings(roomID string, settings *model.RoomSettings) error {
	settings.ID = roomID
	err := db.Save(settings).Error
	return HandleNotFound(err, "room")
}

func UpdateRoomSettings(roomID string, settings map[string]interface{}) (*model.RoomSettings, error) {
	rs := &model.RoomSettings{
		ID: roomID,
	}
	err := db.Model(rs).
		Clauses(clause.Returning{}).
		Updates(settings).Error
	return rs, HandleNotFound(err, "room")
}

func DeleteRoomByID(roomID string) error {
	err := db.Unscoped().Select(clause.Associations).Delete(&model.Room{ID: roomID}).Error
	return HandleNotFound(err, "room")
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
	return HandleNotFound(err, "room")
}

func GetAllRooms(scopes ...func(*gorm.DB) *gorm.DB) ([]*model.Room, error) {
	rooms := []*model.Room{}
	err := db.Scopes(scopes...).Find(&rooms).Error
	return rooms, err
}

func GetAllRoomsCount(scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64
	err := db.Model(&model.Room{}).Scopes(scopes...).Count(&count).Error
	return count, err
}

func GetAllRoomsAndCreator(scopes ...func(*gorm.DB) *gorm.DB) ([]*model.Room, error) {
	rooms := []*model.Room{}
	err := db.Preload("Creator").Scopes(scopes...).Find(&rooms).Error
	return rooms, err
}

func GetAllRoomsByUserID(userID string) ([]*model.Room, error) {
	rooms := []*model.Room{}
	err := db.Where("creator_id = ?", userID).Find(&rooms).Error
	return rooms, err
}

func SetRoomStatus(roomID string, status model.RoomStatus) error {
	err := db.Model(&model.Room{}).Where("id = ?", roomID).Update("status", status).Error
	return HandleNotFound(err, "room")
}

func SetRoomStatusByCreator(userID string, status model.RoomStatus) error {
	return db.Model(&model.Room{}).Where("creator_id = ?", userID).Update("status", status).Error
}
