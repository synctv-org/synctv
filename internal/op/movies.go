package op

import (
	"errors"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
	"gorm.io/gorm"
)

type movies struct {
	roomID string
	cache  rwmap.RWMap[string, *Movie]
}

func (m *movies) AddMovie(mo *model.Movie) error {
	mo.Position = uint(time.Now().UnixMilli())
	movie := &Movie{
		Movie: mo,
	}

	err := movie.Validate()
	if err != nil {
		return err
	}

	err = db.CreateMovie(mo)
	if err != nil {
		return err
	}

	old, ok := m.cache.Swap(mo.ID, movie)
	if ok {
		_ = old.Close()
	}
	return nil
}

func (m *movies) AddMovies(mos []*model.Movie) error {
	inited := make([]*Movie, 0, len(mos))
	for _, mo := range mos {
		mo.Position = uint(time.Now().UnixMilli())
		movie := &Movie{
			Movie: mo,
		}

		err := movie.Validate()
		if err != nil {
			return err
		}

		inited = append(inited, movie)
	}

	err := db.CreateMovies(mos)
	if err != nil {
		return err
	}

	for _, mo := range inited {
		old, ok := m.cache.Swap(mo.Movie.ID, mo)
		if ok {
			_ = old.Close()
		}
	}

	return nil
}

func (m *movies) GetChannel(id string) (*rtmps.Channel, error) {
	if id == "" {
		return nil, errors.New("channel name is nil")
	}
	movie, err := m.GetMovieByID(id)
	if err != nil {
		return nil, err
	}
	return movie.Channel()
}

func (m *movies) Update(movieId string, movie *model.MovieBase) error {
	mv, err := db.GetMovieByID(m.roomID, movieId)
	if err != nil {
		return err
	}
	mv.MovieBase = *movie
	err = db.SaveMovie(mv)
	if err != nil {
		return err
	}
	mm, ok := m.cache.LoadOrStore(mv.ID, &Movie{Movie: mv})
	if ok {
		_ = mm.Close()
	}
	return nil
}

func (m *movies) Clear() error {
	err := db.DeleteMoviesByRoomID(m.roomID)
	if err != nil {
		return err
	}
	return m.Close()
}

func (m *movies) Close() error {
	m.cache.Range(func(key string, value *Movie) bool {
		mm, ok := m.cache.LoadAndDelete(key)
		if ok {
			_ = mm.Close()
		}
		return true
	})
	return nil
}

func (m *movies) DeleteMovieByID(id string) error {
	err := db.DeleteMovieByID(m.roomID, id)
	if err != nil {
		return err
	}
	m.deleteMovieAndChiledCache(id)
	return nil
}

func (m *movies) deleteMovieAndChiledCache(id string) {
	m.cache.Delete(id)
	m.cache.Range(func(key string, value *Movie) bool {
		if value.ParentID == model.EmptyNullString(id) {
			value.Close()
			if value.IsFolder {
				m.deleteMovieAndChiledCache(key)
			} else {
				m.cache.Delete(key)
			}
		}
		return true
	})
}

func (m *movies) DeleteMoviesByID(ids []string) error {
	err := db.DeleteMoviesByID(m.roomID, ids)
	if err != nil {
		return err
	}
	for _, id := range ids {
		m.deleteMovieAndChiledCache(id)
	}
	return nil
}

func (m *movies) GetMovieByID(id string) (*Movie, error) {
	if id == "" {
		return nil, errors.New("movie id is nil")
	}
	mm, ok := m.cache.Load(id)
	if ok {
		return mm, nil
	}
	mv, err := db.GetMovieByID(m.roomID, id)
	if err != nil {
		return nil, err
	}
	mo := &Movie{Movie: mv}
	mm, _ = m.cache.LoadOrStore(mv.ID, mo)
	return mm, nil
}

func (m *movies) SwapMoviePositions(id1, id2 string) error {
	return db.SwapMoviePositions(m.roomID, id1, id2)
}

func (m *movies) GetMoviesWithPage(page, pageSize int, parentID string) ([]*model.Movie, int64, error) {
	scopes := []func(*gorm.DB) *gorm.DB{
		db.WithParentMovieID(parentID),
	}
	count, err := db.GetMoviesCountByRoomID(m.roomID, append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		return nil, 0, err
	}
	movies, err := db.GetMoviesByRoomID(m.roomID, scopes...)
	if err != nil {
		return nil, 0, err
	}
	return movies, count, nil
}
