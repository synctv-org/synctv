package op

import (
	"errors"
	"math/rand"
	"strconv"
	"sync/atomic"
	"time"

	"github.com/google/uuid"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/rwmap"
	"github.com/zijiren233/stream"
)

var roomCache rwmap.RWMap[uint, *Room]

func CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*Room, error) {
	r, err := db.CreateRoom(name, password, conf...)
	if err != nil {
		return nil, err
	}
	return initRoom(r)
}

type RoomConf func(r *Room)

func WithVersion(version uint32) RoomConf {
	return func(r *Room) {
		atomic.StoreUint32(&r.version, version)
	}
}

func initRoom(room *model.Room, conf ...RoomConf) (*Room, error) {
	r := &Room{
		Room:       *room,
		uuid:       uuid.NewMD5(uuid.NameSpaceURL, stream.StringToBytes(strconv.Itoa(int(room.ID)))),
		lastActive: time.Now().UnixMilli(),
		version:    rand.Uint32(),
		current:    newCurrent(),
	}
	for _, c := range conf {
		c(r)
	}
	r, loaded := roomCache.LoadOrStore(room.ID, r)
	if loaded {
		return r, errors.New("room already exists")
	}
	return r, nil
}

func LoadRoom(room *model.Room) (*Room, error) {
	return initRoom(room)
}

func DeleteRoom(room *Room) error {
	room.close()
	roomCache.Delete(room.ID)
	return db.DeleteRoomByID(room.ID)
}

func GetRoomByID(id uint) (*Room, error) {
	r2, ok := roomCache.Load(id)
	if ok {
		return r2, nil
	}
	r, err := db.GetRoomByID(id)
	if err != nil {
		return nil, err
	}
	return initRoom(r)
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
	r, err := GetRoomByID(roomID)
	if err != nil {
		return err
	}
	return r.SetPassword(password)
}

func GetAllRooms() []*Room {
	rooms := make([]*Room, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		rooms = append(rooms, value)
		return true
	})
	return rooms
}

func GetAllRoomsWithNoNeedPassword() []*Room {
	rooms := make([]*Room, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		if !value.NeedPassword() {
			rooms = append(rooms, value)
		}
		return true
	})
	return rooms
}

func GetAllRoomsWithoutHidden() []*Room {
	rooms := make([]*Room, 0, roomCache.Len())
	roomCache.Range(func(key uint, value *Room) bool {
		if !value.Setting.Hidden {
			rooms = append(rooms, value)
		}
		return true
	})
	return rooms
}
