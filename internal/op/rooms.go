package op

import (
	"errors"
	"fmt"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/zijiren233/gencontainer/synccache"
)

var (
	roomCache             *synccache.SyncCache[string, *Room]
	ErrRoomCreatorBanned  = errors.New("room creator is banned")
	ErrRoomCreatorPending = errors.New(
		"room creator is pending approval, please wait for admin to review",
	)
	ErrInvalidRoomID  = errors.New("invalid room ID: must be 32 characters long")
	ErrRoomNotInCache = errors.New("room not found in cache")
)

type RoomEntry = synccache.Entry[*Room]

func RangeRoomCache(f func(key string, value *RoomEntry) bool) {
	roomCache.Range(f)
}

func CreateRoom(
	name, password string,
	maxCount int64,
	conf ...db.CreateRoomConfig,
) (*RoomEntry, error) {
	r, err := db.CreateRoom(name, password, maxCount, conf...)
	if err != nil {
		return nil, err
	}
	return LoadOrInitRoom(r)
}

func checkRoomCreatorStatus(creatorID string) error {
	e, err := LoadOrInitUserByID(creatorID)
	if err != nil {
		return fmt.Errorf("load room creator error: %w", err)
	}

	user := e.Value()
	if user.IsBanned() {
		return ErrRoomCreatorBanned
	}
	if user.IsPending() {
		return ErrRoomCreatorPending
	}
	return nil
}

func LoadOrInitRoom(room *model.Room) (*RoomEntry, error) {
	if err := checkRoomCreatorStatus(room.CreatorID); err != nil {
		return nil, err
	}

	r := &Room{
		Room:    *room,
		current: newCurrent(room.ID, room.Current),
		movies:  &movies{roomID: room.ID},
	}
	r.movies.room = r

	i, _ := roomCache.LoadOrStore(room.ID, r, time.Duration(settings.RoomTTL.Get())*time.Hour)
	return i, nil
}

func DeleteRoomByID(roomID string) error {
	if err := db.DeleteRoomByID(roomID); err != nil {
		return err
	}
	return CloseRoomByID(roomID)
}

func DeleteRoom(room *Room) error {
	if err := db.DeleteRoomByID(room.ID); err != nil {
		return err
	}
	return CloseRoom(room)
}

func DeleteRoomWithRoomEntry(roomE *RoomEntry) error {
	if err := db.DeleteRoomByID(roomE.Value().ID); err != nil {
		return err
	}
	return CloseRoomWithRoomEntry(roomE)
}

func CompareAndDeleteRoom(room *RoomEntry) error {
	if err := db.DeleteRoomByID(room.Value().ID); err != nil {
		return err
	}
	CompareAndCloseRoom(room)
	return nil
}

func CloseRoomByID(roomID string) error {
	if r, loaded := roomCache.LoadAndDelete(roomID); loaded {
		r.Value().close()
	}
	return nil
}

func CloseRoomWithRoomEntry(roomE *synccache.Entry[*Room]) error {
	roomCache.CompareAndDelete(roomE.Value().ID, roomE)
	roomE.Value().close()
	return nil
}

func CloseRoom(room *Room) error {
	roomCache.CompareValueAndDelete(room.ID, room)
	room.close()
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
		return nil, ErrRoomNotInCache
	}

	if err := checkRoomCreatorStatus(r2.Value().CreatorID); err != nil {
		if errors.Is(err, ErrRoomCreatorBanned) || errors.Is(err, ErrRoomCreatorPending) {
			CompareAndCloseRoom(r2)
		}
		return nil, err
	}

	r2.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get()) * time.Hour))
	return r2, nil
}

func LoadOrInitRoomByID(id string) (*RoomEntry, error) {
	if len(id) != 32 {
		return nil, ErrInvalidRoomID
	}

	if i, loaded := roomCache.Load(id); loaded {
		if err := checkRoomCreatorStatus(i.Value().CreatorID); err != nil {
			if errors.Is(err, ErrRoomCreatorBanned) || errors.Is(err, ErrRoomCreatorPending) {
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
	settings, err := db.CreateOrLoadRoomSettings(room.ID)
	if err != nil {
		return nil, err
	}
	room.Settings = settings
	return LoadOrInitRoom(room)
}

func ViewerCount(roomID string) int64 {
	if r, loaded := roomCache.Load(roomID); loaded {
		return r.Value().ViewerCount()
	}
	return 0
}

func SetRoomStatusByID(roomID string, status model.RoomStatus) error {
	room, err := LoadOrInitRoomByID(roomID)
	if err != nil {
		return err
	}
	return room.Value().SetStatus(status)
}
