package op

import (
	"errors"
	"hash/crc32"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/zijiren233/gencontainer/synccache"
)

var roomCache *synccache.SyncCache[string, *Room]

func RangeRoomCache(f func(key string, value *synccache.Entry[*Room]) bool) {
	roomCache.Range(f)
}

func CreateRoom(name, password string, maxCount int64, conf ...db.CreateRoomConfig) (*Room, error) {
	r, err := db.CreateRoom(name, password, maxCount, conf...)
	if err != nil {
		return nil, err
	}
	return LoadOrInitRoom(r)
}

var (
	ErrRoomPending = errors.New("room pending, please wait for admin to approve")
	ErrRoomBanned  = errors.New("room banned")
)

func LoadOrInitRoom(room *model.Room) (*Room, error) {
	switch room.Status {
	case model.RoomStatusBanned:
		return nil, ErrRoomBanned
	case model.RoomStatusPending:
		return nil, ErrRoomPending
	}
	i, _ := roomCache.LoadOrStore(room.ID, &Room{
		Room:    *room,
		version: crc32.ChecksumIEEE(room.HashedPassword),
		current: newCurrent(),
		movies: movies{
			roomID: room.ID,
		},
	}, time.Duration(settings.RoomTTL.Get())*time.Hour)
	return i.Value(), nil
}

func DeleteRoomByID(roomID string) error {
	err := db.DeleteRoomByID(roomID)
	if err != nil {
		return err
	}
	return CloseRoomById(roomID)
}

func CompareAndDeleteRoom(room *Room) error {
	err := db.DeleteRoomByID(room.ID)
	if err != nil {
		return err
	}
	return CompareAndCloseRoom(room)
}

func CloseRoomById(roomID string) error {
	r, loaded := roomCache.LoadAndDelete(roomID)
	if loaded {
		r.Value().close()
	}
	return nil
}

func CompareAndCloseRoomEntry(id string, room *synccache.Entry[*Room]) error {
	if roomCache.CompareAndDelete(id, room) {
		room.Value().close()
	}
	return nil
}

func CompareAndCloseRoom(room *Room) error {
	r, loaded := roomCache.Load(room.ID)
	if loaded {
		if r.Value() != room {
			return errors.New("room compare failed")
		}
		if roomCache.CompareAndDelete(room.ID, r) {
			r.Value().close()
		}
	}
	return nil
}

func LoadRoomByID(id string) (*Room, error) {
	r2, loaded := roomCache.Load(id)
	if loaded {
		r2.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get()) * time.Hour))
		return r2.Value(), nil
	}
	return nil, errors.New("room not found")
}

func LoadOrInitRoomByID(id string) (*Room, error) {
	if len(id) != 32 {
		return nil, errors.New("room id is not 32 bit")
	}
	i, loaded := roomCache.Load(id)
	if loaded {
		i.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get()) * time.Hour))
		return i.Value(), nil
	}
	room, err := db.GetRoomByID(id)
	if err != nil {
		return nil, err
	}
	return LoadOrInitRoom(room)
}

func PeopleNum(roomID string) int64 {
	r, loaded := roomCache.Load(roomID)
	if loaded {
		return r.Value().PeopleNum()
	}
	return 0
}

func GetAllRoomsInCacheWithNoNeedPassword() []*Room {
	rooms := make([]*Room, 0)
	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		v := value.Value()
		if !v.NeedPassword() {
			rooms = append(rooms, v)
		}
		return true
	})
	return rooms
}

func GetAllRoomsInCacheWithoutHidden() []*Room {
	rooms := make([]*Room, 0)
	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		v := value.Value()
		if !v.Settings.Hidden {
			rooms = append(rooms, v)
		}
		return true
	})
	return rooms
}

func SetRoomStatus(roomID string, status model.RoomStatus) error {
	err := db.SetRoomStatus(roomID, status)
	if err != nil {
		return err
	}
	e, loaded := roomCache.LoadAndDelete(roomID)
	if loaded {
		e.Value().close()
	}
	return nil
}
