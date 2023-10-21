package op

import (
	"errors"
	"hash/crc32"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/rwmap"
)

var roomCache rwmap.RWMap[uint, *Room]

func CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*Room, error) {
	r, err := db.CreateRoom(name, password, conf...)
	if err != nil {
		return nil, err
	}
	return InitRoom(r)
}

func InitRoom(room *model.Room) (*Room, error) {
	r := &Room{
		Room:    *room,
		version: crc32.ChecksumIEEE(room.HashedPassword),
		current: newCurrent(),
	}
	r, loaded := roomCache.LoadOrStore(room.ID, r)
	if loaded {
		return r, errors.New("room already init")
	}
	return r, nil
}

func LoadOrInitRoom(room *model.Room) (r *Room, loaded bool) {
	r = &Room{
		Room:    *room,
		version: crc32.ChecksumIEEE(room.HashedPassword),
		current: newCurrent(),
	}
	r, loaded = roomCache.LoadOrStore(room.ID, r)
	return
}

func DeleteRoom(room *Room) error {
	room.close()
	roomCache.Delete(room.ID)
	return db.DeleteRoomByID(room.ID)
}

func DeleteRoomByID(id uint) error {
	r, ok := roomCache.LoadAndDelete(id)
	if ok {
		r.close()
	}

	return db.DeleteRoomByID(r.ID)
}

func LoadRoomByID(id uint) (*Room, error) {
	r2, ok := roomCache.Load(id)
	if ok {
		return r2, nil
	}
	return nil, errors.New("room not found")
}

func LoadOrInitRoomByID(id uint) (*Room, error) {
	r, ok := roomCache.Load(id)
	if ok {
		return r, nil
	}
	room, err := db.GetRoomByID(id)
	if err != nil {
		return nil, err
	}
	r, _ = LoadOrInitRoom(room)
	return r, nil
}

func HasRoom(roomID uint) bool {
	_, ok := roomCache.Load(roomID)
	if ok {
		return true
	}
	ok, err := db.HasRoom(roomID)
	if err != nil {
		return false
	}
	return ok
}

func HasRoomByName(name string) bool {
	ok, err := db.HasRoomByName(name)
	if err != nil {
		return false
	}
	return ok
}

func SetRoomPassword(roomID uint, password string) error {
	r, err := LoadOrInitRoomByID(roomID)
	if err != nil {
		return err
	}
	return r.SetPassword(password)
}

func GetAllRoomsInCache() []*Room {
	rooms := make([]*Room, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		rooms = append(rooms, value)
		return true
	})
	return rooms
}

func GetAllRoomsInCacheWithNoNeedPassword() []*Room {
	rooms := make([]*Room, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		if !value.NeedPassword() {
			rooms = append(rooms, value)
		}
		return true
	})
	return rooms
}

func GetAllRoomsInCacheWithoutHidden() []*Room {
	rooms := make([]*Room, 0, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		if !value.Settings.Hidden {
			rooms = append(rooms, value)
		}
		return true
	})
	return rooms
}
