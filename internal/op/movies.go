package op

import (
	"errors"
	"sync"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/gencontainer/dllist"
	rtmps "github.com/zijiren233/livelib/server"
)

type movies struct {
	roomID uint
	lock   sync.RWMutex
	list   dllist.Dllist[*movie]
	once   sync.Once
}

func (m *movies) init() {
	m.once.Do(func() {
		for _, m2 := range db.GetAllMoviesByRoomID(m.roomID) {
			m.list.PushBack(&movie{
				Movie: m2,
			})
		}
	})
}

func (m *movies) Len() int {
	m.lock.RLock()
	defer m.lock.RUnlock()
	m.init()
	return m.list.Len()
}

func (m *movies) Add(mo *model.Movie) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.init()
	mo.Position = uint(time.Now().UnixMilli())
	movie := &movie{
		Movie: mo,
	}

	// You need to init to get the pullKey first, and then create it into the database.
	err := movie.init()
	if err != nil {
		return err
	}

	err = db.CreateMovie(mo)
	if err != nil {
		movie.terminate()
		return err
	}

	m.list.PushBack(movie)
	return nil
}

func (m *movies) GetChannel(channelName string) (*rtmps.Channel, error) {
	if channelName == "" {
		return nil, errors.New("channel name is nil")
	}
	m.lock.RLock()
	defer m.lock.RUnlock()
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.PullKey == channelName {
			return e.Value.Channel()
		}
	}
	return nil, errors.New("channel not found")
}

func (m *movies) Update(movieId uint, movie model.BaseMovieInfo) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.ID == movieId {
			err := e.Value.Update(movie)
			if err != nil {
				return err
			}
			return db.SaveMovie(e.Value.Movie)
		}
	}
	return nil
}

func (m *movies) Clear() error {
	m.lock.Lock()
	defer m.lock.Unlock()
	err := db.DeleteMoviesByRoomID(m.roomID)
	if err != nil {
		return err
	}
	for e := m.list.Front(); e != nil; e = e.Next() {
		e.Value.Terminate()
	}
	m.list.Clear()
	return nil
}

func (m *movies) Close() error {
	m.lock.Lock()
	defer m.lock.Unlock()
	for e := m.list.Front(); e != nil; e = e.Next() {
		e.Value.Terminate()
	}
	m.list.Clear()
	return nil
}

func (m *movies) DeleteMovieByID(id uint) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.init()

	err := db.DeleteMovieByID(m.roomID, id)
	if err != nil {
		return err
	}

	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.ID == id {
			m.list.Remove(e).Terminate()
			return nil
		}
	}
	return errors.New("movie not found")
}

func (m *movies) GetMovieByID(id uint) (*movie, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()
	return m.getMovieByID(id)
}

func (m *movies) getMovieByID(id uint) (*movie, error) {
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.ID == id {
			return e.Value, nil
		}
	}
	return nil, errors.New("movie not found")
}

func (m *movies) getMovieElementByID(id uint) (*dllist.Element[*movie], error) {
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.ID == id {
			return e, nil
		}
	}
	return nil, errors.New("movie not found")
}

func (m *movies) GetMovieWithPullKey(pullKey string) (*movie, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()
	m.init()

	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.PullKey == pullKey {
			return e.Value, nil
		}
	}
	return nil, errors.New("movie not found")
}

func (m *movies) SwapMoviePositions(id1, id2 uint) error {
	m.lock.Lock()
	defer m.lock.Unlock()
	m.init()

	err := db.SwapMoviePositions(m.roomID, id1, id2)
	if err != nil {
		return err
	}

	movie1, err := m.getMovieElementByID(id1)
	if err != nil {
		return err
	}

	movie2, err := m.getMovieElementByID(id2)
	if err != nil {
		return err
	}

	movie1.Value.Position, movie2.Value.Position = movie2.Value.Position, movie1.Value.Position

	m.list.Swap(movie1, movie2)
	return nil
}

func (m *movies) GetMoviesWithPage(page, pageSize int) []*movie {
	m.lock.RLock()
	defer m.lock.RUnlock()
	m.init()

	start, end := utils.GetPageItemsRange(m.list.Len(), page, pageSize)
	ms := make([]*movie, 0, end-start)
	i := 0
	for e := m.list.Front(); e != nil; e = e.Next() {
		if i >= start && i < end {
			ms = append(ms, e.Value)
		} else if i >= end {
			return ms
		}
		i++
	}
	return ms
}
