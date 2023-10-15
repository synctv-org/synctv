package room

import (
	"errors"
	"time"

	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
)

const (
	roomMaxInactivityTime = time.Hour * 12
)

var (
	ErrRoomNotFound     = errors.New("room not found")
	ErrUserNotFound     = errors.New("user not found")
	ErrRoomAlreadyExist = errors.New("room already exist")
)

type Rooms struct {
	rooms rwmap.RWMap[string, *Room]
}

func NewRooms() *Rooms {
	return &Rooms{}
}

func (rs *Rooms) List() (rooms []*Room) {
	rooms = make([]*Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *Room) bool {
		rooms = append(rooms, r)
		return true
	})
	return
}

func (rs *Rooms) ListNonHidden() (rooms []*Room) {
	rooms = make([]*Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *Room) bool {
		if !r.Hidden() {
			rooms = append(rooms, r)
		}
		return true
	})
	return
}

func (rs *Rooms) ListHidden() (rooms []*Room) {
	rooms = make([]*Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *Room) bool {
		if r.Hidden() {
			rooms = append(rooms, r)
		}
		return true
	})
	return
}

func (rs *Rooms) HasRoom(id string) bool {
	_, ok := rs.rooms.Load(id)
	return ok
}

func (rs *Rooms) GetRoom(id string) (*Room, error) {
	if id == "" {
		return nil, ErrRoomIdEmpty
	}
	r, ok := rs.rooms.Load(id)
	if !ok {
		return nil, ErrRoomNotFound
	}
	return r, nil
}

func (rs *Rooms) CreateRoom(id string, password string, s *rtmps.Server, conf ...RoomConf) (*Room, error) {
	r, err := NewRoom(id, password, s, conf...)
	if err != nil {
		return nil, err
	}
	r, loaded := rs.rooms.LoadOrStore(r.Id(), r)
	if loaded {
		return nil, ErrRoomAlreadyExist
	}
	return r, nil
}

func (rs *Rooms) DelRoom(id string) error {
	if id == "" {
		return ErrRoomIdEmpty
	}
	r, ok := rs.rooms.LoadAndDelete(id)
	if !ok {
		return ErrRoomNotFound
	}
	return r.Close()
}
