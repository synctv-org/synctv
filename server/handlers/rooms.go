package handlers

import (
	"errors"
	"sync"
	"time"

	"github.com/synctv-org/synctv/room"
	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
)

const (
	roomMaxInactivityTime = time.Hour * 12
)

var (
	Rooms    *rooms
	once     = sync.Once{}
	initOnce = func() {
		once.Do(func() {
			Rooms = newRooms()
		})
	}
)

var (
	ErrRoomIDEmpty      = errors.New("roomid is empty")
	ErrRoomNotFound     = errors.New("room not found")
	ErrUserNotFound     = errors.New("user not found")
	ErrRoomAlreadyExist = errors.New("room already exist")
)

type rooms struct {
	rooms rwmap.RWMap[string, *room.Room]
}

func newRooms() *rooms {
	return &rooms{}
}

func (rs *rooms) List() (rooms []*room.Room) {
	rooms = make([]*room.Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *room.Room) bool {
		rooms = append(rooms, r)
		return true
	})
	return
}

func (rs *rooms) ListNonHidden() (rooms []*room.Room) {
	rooms = make([]*room.Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *room.Room) bool {
		if !r.Hidden() {
			rooms = append(rooms, r)
		}
		return true
	})
	return
}

func (rs *rooms) ListHidden() (rooms []*room.Room) {
	rooms = make([]*room.Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *room.Room) bool {
		if r.Hidden() {
			rooms = append(rooms, r)
		}
		return true
	})
	return
}

func (rs *rooms) HasRoom(id string) bool {
	_, ok := rs.rooms.Load(id)
	return ok
}

func (rs *rooms) GetRoom(id string) (*room.Room, error) {
	if id == "" {
		return nil, ErrRoomIDEmpty
	}
	r, ok := rs.rooms.Load(id)
	if !ok {
		return nil, ErrRoomNotFound
	}
	return r, nil
}

func (rs *rooms) CreateRoom(id string, password string, s *rtmps.Server, conf ...room.RoomConf) (*room.Room, error) {
	r, err := room.NewRoom(id, password, s, conf...)
	if err != nil {
		return nil, err
	}
	r, loaded := rs.rooms.LoadOrStore(r.ID(), r)
	if loaded {
		return nil, ErrRoomAlreadyExist
	}
	return r, nil
}

func (rs *rooms) DelRoom(id string) error {
	if id == "" {
		return ErrRoomIDEmpty
	}
	r, ok := rs.rooms.LoadAndDelete(id)
	if !ok {
		return ErrRoomNotFound
	}
	return r.Close()
}
