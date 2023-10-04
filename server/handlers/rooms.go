package handlers

import (
	"errors"
	"sync"
	"time"

	"github.com/synctv-org/synctv/room"
	"github.com/zijiren233/gencontainer/rwmap"
	"github.com/zijiren233/ksync"
	rtmps "github.com/zijiren233/livelib/server"
)

const (
	roomMaxInactivityTime = time.Hour * 12
)

var (
	Rooms    *rooms
	initOnce = sync.OnceFunc(func() {
		Rooms = newRooms()
	})
)

var (
	ErrRoomIDEmpty      = errors.New("roomid is empty")
	ErrRoomNotFound     = errors.New("room not found")
	ErrUserNotFound     = errors.New("user not found")
	ErrRoomAlreadyExist = errors.New("room already exist")
)

type rooms struct {
	rooms *rwmap.RWMap[string, *room.Room]
	lock  *ksync.Krwmutex
}

func newRooms() *rooms {
	return &rooms{
		rooms: &rwmap.RWMap[string, *room.Room]{},
		lock:  ksync.NewKrwmutex(),
	}
}

func (rs *rooms) List() (rooms []*room.Room) {
	rooms = make([]*room.Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *room.Room) bool {
		rs.lock.RLock(id)
		defer rs.lock.RUnlock(id)
		if r.Closed() {
			rs.rooms.Delete(id)
			return true
		}
		rooms = append(rooms, r)
		return true
	})
	return
}

func (rs *rooms) ListNonHidden() (rooms []*room.Room) {
	rooms = make([]*room.Room, 0, rs.rooms.Len())
	rs.rooms.Range(func(id string, r *room.Room) bool {
		rs.lock.RLock(id)
		defer rs.lock.RUnlock(id)
		if r.Closed() {
			rs.rooms.Delete(id)
			return true
		}
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
		rs.lock.RLock(id)
		defer rs.lock.RUnlock(id)
		if r.Closed() {
			rs.rooms.Delete(id)
			return true
		}
		if r.Hidden() {
			rooms = append(rooms, r)
		}
		return true
	})
	return
}

func (rs *rooms) HasRoom(id string) bool {
	rs.lock.Lock(id)
	defer rs.lock.Unlock(id)
	r, ok := rs.rooms.Load(id)
	if !ok || r.Closed() {
		return false
	}
	return ok
}

func (rs *rooms) GetRoom(id string) (*room.Room, error) {
	if id == "" {
		return nil, ErrRoomIDEmpty
	}
	rs.lock.RLock(id)
	defer rs.lock.RUnlock(id)
	r, ok := rs.rooms.Load(id)
	if !ok || r.Closed() {
		return nil, ErrRoomNotFound
	}
	return r, nil
}

func (rs *rooms) CreateRoom(id string, password string, s *rtmps.Server, conf ...room.RoomConf) (*room.Room, error) {
	if id == "" {
		return nil, ErrRoomIDEmpty
	}
	rs.lock.Lock(id)
	defer rs.lock.Unlock(id)

	if oldR, ok := rs.rooms.Load(id); ok && !oldR.Closed() {
		return nil, ErrRoomAlreadyExist
	}
	r, err := room.NewRoom(id, password, s, conf...)
	if err != nil {
		return nil, err
	}
	rs.rooms.Store(id, r)
	return r, nil
}

func (rs *rooms) DelRoom(id string) error {
	if id == "" {
		return ErrRoomIDEmpty
	}
	rs.lock.Lock(id)
	defer rs.lock.Unlock(id)
	r, ok := rs.rooms.LoadAndDelete(id)
	if !ok {
		return ErrRoomNotFound
	}
	return r.Close()
}
