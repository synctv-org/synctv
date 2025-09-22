package db

import (
	"errors"
	"fmt"

	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

const (
	ErrRoomNotFound = "room"
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
		r.RoomMembers = []*model.RoomMember{
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
		r.RoomMembers = append(r.RoomMembers, relations...)
	}
}

func WithStatus(status model.RoomStatus) CreateRoomConfig {
	return func(r *model.Room) {
		r.Status = status
	}
}

func WithSettingHidden(hidden bool) CreateRoomConfig {
	return func(r *model.Room) {
		if r.Settings == nil {
			r.Settings = model.DefaultRoomSettings()
		}

		r.Settings.Hidden = hidden
	}
}

// if maxCount is 0, it will be ignored
func CreateRoom(
	name, password string,
	maxCount int64,
	conf ...CreateRoomConfig,
) (*model.Room, error) {
	r := &model.Room{
		Name:     name,
		Settings: model.DefaultRoomSettings(),
	}
	for _, c := range conf {
		c(r)
	}

	if password != "" {
		hashedPassword, err := bcrypt.GenerateFromPassword(
			stream.StringToBytes(password),
			bcrypt.DefaultCost,
		)
		if err != nil {
			return nil, fmt.Errorf("failed to hash password: %w", err)
		}

		r.HashedPassword = hashedPassword
	}

	err := Transactional(func(tx *gorm.DB) error {
		if maxCount > 0 {
			var count int64
			if err := tx.Model(&model.Room{}).Where("creator_id = ?", r.CreatorID).Count(&count).Error; err != nil {
				return fmt.Errorf("failed to count rooms: %w", err)
			}

			if count >= maxCount {
				return errors.New("room count exceeds limit")
			}
		}

		if err := tx.Create(r).Error; err != nil {
			if errors.Is(err, gorm.ErrDuplicatedKey) {
				return errors.New("room already exists")
			}
			return fmt.Errorf("failed to create room: %w", err)
		}

		return nil
	})
	if err != nil {
		return nil, err
	}

	return r, nil
}

func GetRoomByID(id string) (*model.Room, error) {
	if len(id) != 32 {
		return nil, errors.New("room id is not 32 bit")
	}

	var r model.Room

	err := db.Where("id = ?", id).First(&r).Error

	return &r, HandleNotFound(err, ErrRoomNotFound)
}

func CreateOrLoadRoomSettings(roomID string) (*model.RoomSettings, error) {
	var rs model.RoomSettings

	err := db.Where(model.RoomSettings{ID: roomID}).
		Attrs(model.DefaultRoomSettings()).
		FirstOrCreate(&rs).
		Error

	return &rs, err
}

func SaveRoomSettings(roomID string, settings *model.RoomSettings) error {
	settings.ID = roomID
	return HandleNotFound(db.Save(settings).Error, "room settings")
}

func UpdateRoomSettings(roomID string, settings map[string]any) (*model.RoomSettings, error) {
	var rs model.RoomSettings

	err := db.Model(&model.RoomSettings{ID: roomID}).
		Clauses(clause.Returning{}).
		Updates(settings).
		First(&rs).Error

	return &rs, HandleNotFound(err, "room settings")
}

func DeleteRoomByID(roomID string) error {
	result := db.Unscoped().Select(clause.Associations).Delete(&model.Room{ID: roomID})
	return HandleUpdateResult(result, ErrRoomNotFound)
}

func SetRoomPassword(roomID, password string) error {
	var (
		hashedPassword []byte
		err            error
	)

	if password != "" {
		hashedPassword, err = bcrypt.GenerateFromPassword(
			stream.StringToBytes(password),
			bcrypt.DefaultCost,
		)
		if err != nil {
			return fmt.Errorf("failed to hash password: %w", err)
		}
	}

	return SetRoomHashedPassword(roomID, hashedPassword)
}

func SetRoomHashedPassword(roomID string, hashedPassword []byte) error {
	result := db.Model(&model.Room{}).
		Where("id = ?", roomID).
		Update("hashed_password", hashedPassword)
	return HandleUpdateResult(result, ErrRoomNotFound)
}

func GetAllRooms(scopes ...func(*gorm.DB) *gorm.DB) ([]*model.Room, error) {
	var rooms []*model.Room

	err := db.Scopes(scopes...).Find(&rooms).Error
	return rooms, err
}

func GetAllRoomsCount(scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64

	err := db.Model(&model.Room{}).Scopes(scopes...).Count(&count).Error
	return count, err
}

func GetAllRoomsAndCreator(scopes ...func(*gorm.DB) *gorm.DB) ([]*model.Room, error) {
	var rooms []*model.Room

	err := db.Preload("Creator").Scopes(scopes...).Find(&rooms).Error
	return rooms, err
}

func GetAllRoomsByUserID(userID string) ([]*model.Room, error) {
	var rooms []*model.Room

	err := db.Where("creator_id = ?", userID).Find(&rooms).Error
	return rooms, err
}

func SetRoomStatus(roomID string, status model.RoomStatus) error {
	result := db.Model(&model.Room{}).Where("id = ?", roomID).Update("status", status)
	return HandleUpdateResult(result, ErrRoomNotFound)
}

func SetRoomStatusByCreator(userID string, status model.RoomStatus) error {
	result := db.Model(&model.Room{}).Where("creator_id = ?", userID).Update("status", status)
	return HandleUpdateResult(result, ErrRoomNotFound)
}

func SetRoomCurrent(roomID string, current *model.Current) error {
	r := &model.Room{
		Current: current,
	}
	result := db.Model(r).
		Where("id = ?", roomID).
		Select("Current").
		Updates(r)

	return HandleUpdateResult(result, ErrRoomNotFound)
}
