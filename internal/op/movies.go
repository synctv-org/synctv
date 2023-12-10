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
	roomID string
	lock   sync.RWMutex
	list   dllist.Dllist[*Movie]
	once   sync.Once
}

func (m *movies) init() {
	m.once.Do(func() {
		for _, m2 := range db.GetAllMoviesByRoomID(m.roomID) {
			m.list.PushBack(&Movie{
				Movie: *m2,
				lock:  new(sync.RWMutex),
				Cache: newBaseCache(),
			})
		}
	})
}

func (m *movies) Len() int {
	m.init()
	m.lock.RLock()
	defer m.lock.RUnlock()
	return m.list.Len()
}

func (m *movies) AddMovie(mo *model.Movie) error {
	m.init()
	m.lock.Lock()
	defer m.lock.Unlock()
	mo.Position = uint(time.Now().UnixMilli())
	movie := &Movie{
		Movie: *mo,
		lock:  new(sync.RWMutex),
		Cache: newBaseCache(),
	}

	err := movie.init()
	if err != nil {
		return err
	}

	err = db.CreateMovie(mo)
	if err != nil {
		movie.terminate()
		return err
	}

	movie.Movie.ID = mo.ID
	m.list.PushBack(movie)
	return nil
}

func (m *movies) AddMovies(mos []*model.Movie) error {
	m.init()
	m.lock.Lock()
	defer m.lock.Unlock()
	inited := make([]*Movie, 0, len(mos))
	for _, mo := range mos {
		mo.Position = uint(time.Now().UnixMilli())
		movie := &Movie{
			Movie: *mo,
			lock:  new(sync.RWMutex),
			Cache: newBaseCache(),
		}

		err := movie.init()
		if err != nil {
			for _, movie := range inited {
				movie.terminate()
			}
			return err
		}

		inited = append(inited, movie)
	}

	err := db.CreateMovies(mos)
	if err != nil {
		for _, movie := range inited {
			movie.terminate()
		}
		return err
	}

	for i, mo := range inited {
		mo.Movie.ID = mos[i].ID
		m.list.PushBack(mo)
	}

	return nil
}

func (m *movies) GetChannel(id string) (*rtmps.Channel, error) {
	if id == "" {
		return nil, errors.New("channel name is nil")
	}
	m.init()
	m.lock.RLock()
	defer m.lock.RUnlock()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.Movie.ID == id {
			return e.Value.Channel()
		}
	}
	return nil, errors.New("channel not found")
}

func (m *movies) Update(movieId string, movie *model.BaseMovie) error {
	m.init()
	m.lock.Lock()
	defer m.lock.Unlock()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.Movie.ID == movieId {
			err := e.Value.Update(movie)
			if err != nil {
				return err
			}
			return db.SaveMovie(&e.Value.Movie)
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

func (m *movies) DeleteMovieByID(id string) error {
	m.init()
	m.lock.Lock()
	defer m.lock.Unlock()

	err := db.DeleteMovieByID(m.roomID, id)
	if err != nil {
		return err
	}

	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.Movie.ID == id {
			m.list.Remove(e).Terminate()
			return nil
		}
	}
	return errors.New("movie not found")
}

func (m *movies) GetMovieByID(id string) (*Movie, error) {
	m.lock.RLock()
	defer m.lock.RUnlock()
	return m.getMovieByID(id)
}

func (m *movies) getMovieByID(id string) (*Movie, error) {
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.Movie.ID == id {
			return e.Value, nil
		}
	}
	return nil, errors.New("movie not found")
}

func (m *movies) getMovieElementByID(id string) (*dllist.Element[*Movie], error) {
	m.init()
	for e := m.list.Front(); e != nil; e = e.Next() {
		if e.Value.Movie.ID == id {
			return e, nil
		}
	}
	return nil, errors.New("movie not found")
}

func (m *movies) SwapMoviePositions(id1, id2 string) error {
	m.init()
	m.lock.Lock()
	defer m.lock.Unlock()

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

	movie1.Value.Movie.Position, movie2.Value.Movie.Position = movie2.Value.Movie.Position, movie1.Value.Movie.Position

	m.list.Swap(movie1, movie2)
	return nil
}

func (m *movies) GetMoviesWithPage(page, pageSize int) []*Movie {
	m.init()
	m.lock.RLock()
	defer m.lock.RUnlock()

	start, end := utils.GetPageItemsRange(m.list.Len(), page, pageSize)
	ms := make([]*Movie, 0, end-start)
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
