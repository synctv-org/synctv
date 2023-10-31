package op

import (
	"errors"
	"hash/crc32"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	synccache "github.com/synctv-org/synctv/utils/syncCache"
	"github.com/zijiren233/gencontainer/heap"
)

var roomCache *synccache.SyncCache[string, *Room]

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
		movies: movies{
			roomID: room.ID,
		},
	}

	i, loaded := roomCache.LoadOrStore(room.ID, r, time.Duration(settings.RoomTTL.Get()))
	if loaded {
		return r, errors.New("room already init")
	}
	return i.Value(), nil
}

func LoadOrInitRoom(room *model.Room) (*Room, error) {
	t := time.Duration(settings.RoomTTL.Get())
	i, loaded := roomCache.LoadOrStore(room.ID, &Room{
		Room:    *room,
		version: crc32.ChecksumIEEE(room.HashedPassword),
		current: newCurrent(),
		movies: movies{
			roomID: room.ID,
		},
	}, t)
	if loaded {
		i.SetExpiration(time.Now().Add(t))
	}
	switch room.Status {
	case model.RoomStatusBanned:
		return nil, errors.New("room banned")
	case model.RoomStatusPending:
		return nil, errors.New("room pending, please wait for admin to approve")
	case model.RoomStatusStopped:
		return nil, errors.New("room stopped")
	}
	return i.Value(), nil
}

func DeleteRoom(roomID string) error {
	err := db.DeleteRoomByID(roomID)
	if err != nil {
		return err
	}
	return CloseRoom(roomID)
}

func CloseRoom(roomID string) error {
	r, loaded := roomCache.LoadAndDelete(roomID)
	if loaded {
		r.Value().close()
	}
	return nil
}

func LoadRoomByID(id string) (*Room, error) {
	r2, loaded := roomCache.Load(id)
	if loaded {
		r2.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get())))
		return r2.Value(), nil
	}
	return nil, errors.New("room not found")
}

func LoadOrInitRoomByID(id string) (*Room, error) {
	if len(id) != 36 {
		return nil, errors.New("room id is not 32 bit")
	}
	i, loaded := roomCache.Load(id)
	if loaded {
		i.SetExpiration(time.Now().Add(time.Duration(settings.RoomTTL.Get())))
		return i.Value(), nil
	}
	room, err := db.GetRoomByID(id)
	if err != nil {
		return nil, err
	}
	return LoadOrInitRoom(room)
}

func ClientNum(roomID string) int64 {
	r, loaded := roomCache.Load(roomID)
	if loaded {
		return r.Value().ClientNum()
	}
	return 0
}

func HasRoom(roomID string) bool {
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

func SetRoomPassword(roomID, password string) error {
	r, err := LoadOrInitRoomByID(roomID)
	if err != nil {
		return err
	}
	return r.SetPassword(password)
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

type RoomHeapItem struct {
	ID           string
	RoomName     string
	ClientNum    int64
	NeedPassword bool
	CreatorID    string
	CreatedAt    time.Time
}

type RoomHeap []*RoomHeapItem

func (h RoomHeap) Len() int {
	return len(h)
}

func (h RoomHeap) Less(i, j int) bool {
	return h[i].ClientNum < h[j].ClientNum
}

func (h RoomHeap) Swap(i, j int) {
	h[i], h[j] = h[j], h[i]
}

func (h *RoomHeap) Push(x *RoomHeapItem) {
	*h = append(*h, x)
}

func (h *RoomHeap) Pop() *RoomHeapItem {
	old := *h
	n := len(old)
	x := old[n-1]
	*h = old[0 : n-1]
	return x
}

func GetRoomHeapInCacheWithoutHidden() RoomHeap {
	rooms := make(RoomHeap, 0)
	roomCache.Range(func(key string, value *synccache.Entry[*Room]) bool {
		v := value.Value()
		if !v.Settings.Hidden {
			heap.Push[*RoomHeapItem](&rooms, &RoomHeapItem{
				ID:           v.ID,
				RoomName:     v.Name,
				ClientNum:    v.ClientNum(),
				NeedPassword: v.NeedPassword(),
				CreatorID:    v.CreatorID,
				CreatedAt:    v.CreatedAt,
			})
		}
		return true
	})
	return rooms
}
