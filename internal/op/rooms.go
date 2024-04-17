package op

import (
	"errors"
	"fmt"
	"hash/crc32"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/zijiren233/gencontainer/synccache"
)

var roomCache *synccache.SyncCache[string, *Room]

type RoomEntry = synccache.Entry[*Room]

func RangeRoomCache(f func(key string, value *synccache.Entry[*Room]) bool) {
	roomCache.Range(f)
}

func CreateRoom(name, password string, maxCount int64, conf ...db.CreateRoomConfig) (*RoomEntry, error) {
	r, err := db.CreateRoom(name, password, maxCount, conf...)
	if err != nil {
		return nil, err
	}
	return LoadOrInitRoom(r)
}

var (
	ErrRoomPending          = errors.New("room pending, please wait for admin to approve")
	ErrRoomBanned           = errors.New("room banned")
	ErrRoomCreatorBanned    = errors.New("room creator banned")
	ErrorRoomCreatorPending = errors.New("room creator pending, please wait for admin to approve")
)

func checkRoomCreatorStatus(creatorID string) error {
	e, err := LoadOrInitUserByID(creatorID)
	if err != nil {
		return fmt.Errorf("load room creator error: %w", err)
	}

	if e.Value().IsBanned() {
		return ErrRoomCreatorBanned
	}
	if e.Value().IsPending() {
		return ErrorRoomCreatorPending
	}
	return nil
}

func LoadOrInitRoom(room *model.Room) (*RoomEntry, error) {
	switch room.Status {
	case model.RoomStatusBanned:
		return nil, ErrRoomBanned
	case model.RoomStatusPending:
		return nil, ErrRoomPending
	}

	err := checkRoomCreatorStatus(room.CreatorID)
	if err != nil {
		return nil, err
	}

	i, _ := roomCache.LoadOrStore(room.ID, &Room{
		Room:    *room,
		version: crc32.ChecksumIEEE(room.HashedPassword),
		current: newCurrent(),
		movies: movies{
			roomID: room.ID,
		},
	}, time.Duration(settings.RoomTTL.Get())*time.Hour)
	return i, nil
}

func DeleteRoomByID(roomID string) error {
	err := db.DeleteRoomByID(roomID)
	if err != nil {
		return err
	}
	return CloseRoomById(roomID)
}

func CompareAndDeleteRoom(room *RoomEntry) error {
	err := db.DeleteRoomByID(room.Value().ID)
	if err != nil {
		return err
	}
	CompareAndCloseRoom(room)
	return nil
}

func CloseRoomById(roomID string) error {
	r, loaded := roomCache.LoadAndDelete(roomID)
	if loaded {
		r.Value().close()
	}
	return nil
}

func CompareAndCloseRoom(room *RoomEntry) bool {
	if roomCache.CompareAndDelete(room.Value().ID, room) {
		room.Value().close()
		return true
	}
	return false
}

func LoadRoomByID(id string) (*RoomEntry, error) {
	r2, loaded := roomCache.Load(id)
	if !loaded {
		return nil, errors.New("room not found")
	}

	err := checkRoomCreatorStatus(r2.Value().CreatorID)
	if err != nil {
		if errors.Is(err, ErrRoomCreatorBanned) || errors.Is(err, ErrorRoomCreatorPending) {
			CompareAndCloseRoom(r2)
		}
		return nil, err
	}

	r2.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get()) * time.Hour))
	return r2, nil
}

func LoadOrInitRoomByID(id string) (*RoomEntry, error) {
	if len(id) != 32 {
		return nil, errors.New("room id is not 32 bit")
	}
	i, loaded := roomCache.Load(id)
	if loaded {
		err := checkRoomCreatorStatus(i.Value().CreatorID)
		if err != nil {
			if errors.Is(err, ErrRoomCreatorBanned) || errors.Is(err, ErrorRoomCreatorPending) {
				CompareAndCloseRoom(i)
			}
			return nil, err
		}
		i.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get()) * time.Hour))
		return i, nil
	}
	room, err := db.GetRoomByID(id)
	if err != nil {
		return nil, err
	}
	settings, err := db.GetOrCreateRoomSettings(room.ID)
	if err != nil {
		return nil, err
	}
	room.Settings = settings
	return LoadOrInitRoom(room)
}

func PeopleNum(roomID string) int64 {
	r, loaded := roomCache.Load(roomID)
	if loaded {
		return r.Value().PeopleNum()
	}
	return 0
}

func SetRoomStatusByID(roomID string, status model.RoomStatus) error {
	err := db.SetRoomStatus(roomID, status)
	if err != nil {
		return err
	}
	switch status {
	case model.RoomStatusBanned, model.RoomStatusPending:
		roomCache.Delete(roomID)
	}
	return nil
}
