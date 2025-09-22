package op

import (
	"errors"
	"fmt"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
	"gorm.io/gorm"
)

type movies struct {
	roomID string
	room   *Room
	cache  rwmap.RWMap[string, *Movie]
}

//nolint:gosec
func (m *movies) AddMovie(mo *model.Movie) error {
	mo.Position = uint(time.Now().UnixMilli())
	movie := &Movie{
		room:  m.room,
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

//nolint:gosec
func (m *movies) AddMovies(mos []*model.Movie) error {
	inited := make([]*Movie, 0, len(mos))
	for _, mo := range mos {
		mo.Position = uint(time.Now().UnixMilli())
		movie := &Movie{
			room:  m.room,
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
		old, ok := m.cache.Swap(mo.ID, mo)
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

func (m *movies) Update(movieID string, movie *model.MovieBase) error {
	mv, err := db.GetMovieByID(m.roomID, movieID)
	if err != nil {
		return err
	}

	mv.MovieBase = *movie

	err = db.SaveMovie(mv)
	if err != nil {
		return err
	}

	mm, loaded := m.cache.LoadAndDelete(mv.ID)
	if loaded {
		_ = mm.Close()
	}

	return nil
}

func (m *movies) Clear() error {
	return m.DeleteMovieByParentID("")
}

func (m *movies) ClearCache() {
	m.cache.Range(func(key string, value *Movie) bool {
		m.cache.CompareAndDelete(key, value)
		value.Close()
		return true
	})
}

func (m *movies) Close() error {
	m.ClearCache()
	return nil
}

func (m *movies) DeleteMovieByParentID(parentID string) error {
	err := db.DeleteMoviesByRoomIDAndParentID(m.roomID, parentID)
	if err != nil {
		return err
	}

	m.DeleteMovieAndChiledCache(parentID)

	return nil
}

func (m *movies) DeleteMovieByID(id string) error {
	err := db.DeleteMovieByID(m.roomID, id)
	if err != nil {
		return err
	}

	m.DeleteMovieAndChiledCache(id)

	return nil
}

func (m *movies) DeleteMovieAndChiledCache(id ...string) {
	idm := make(map[model.EmptyNullString]struct{}, len(id))
	for _, id := range id {
		idm[model.EmptyNullString(id)] = struct{}{}
	}

	if _, ok := idm[model.EmptyNullString("")]; ok {
		m.ClearCache()
		return
	}

	m.deleteMovieAndChiledCache(idm)
}

func (m *movies) deleteMovieAndChiledCache(ids map[model.EmptyNullString]struct{}) {
	next := make(map[model.EmptyNullString]struct{})
	m.cache.Range(func(key string, value *Movie) bool {
		if _, ok := ids[value.ParentID]; ok {
			if value.IsFolder {
				next[model.EmptyNullString(value.ID)] = struct{}{}
			}

			m.cache.CompareAndDelete(key, value)
			value.Close()
		}

		return true
	})

	if len(next) > 0 {
		m.deleteMovieAndChiledCache(next)
	}
}

func (m *movies) DeleteMoviesByID(ids []string) error {
	err := db.DeleteMoviesByID(m.roomID, ids)
	if err != nil {
		return err
	}

	m.DeleteMovieAndChiledCache(ids...)

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

	mm, _ = m.cache.LoadOrStore(mv.ID, &Movie{
		room:  m.room,
		Movie: mv,
	})

	return mm, nil
}

func (m *movies) SwapMoviePositions(id1, id2 string) error {
	return db.SwapMoviePositions(m.roomID, id1, id2)
}

func (m *movies) GetMoviesWithPage(
	keyword string,
	page, pageSize int,
	parentID string,
) ([]*model.Movie, int64, error) {
	scopes := []func(*gorm.DB) *gorm.DB{
		db.WithParentMovieID(parentID),
	}
	if keyword != "" {
		scopes = append(scopes, db.WhereMovieNameLikeOrURLLike(keyword, keyword))
	}

	count, err := db.GetMoviesCountByRoomID(
		m.roomID,
		append(scopes, db.Paginate(page, pageSize))...)
	if err != nil {
		return nil, 0, err
	}

	movies, err := db.GetMoviesByRoomID(m.roomID, scopes...)
	if err != nil {
		return nil, 0, err
	}

	return movies, count, nil
}

// IsParentOf check if parentID is the parent of id
func (m *movies) IsParentOf(id, parentID string) (bool, error) {
	if parentID == "" {
		return id != "", nil
	}

	mv, err := m.GetMovieByID(parentID)
	if err != nil {
		return false, fmt.Errorf("get parent movie failed: %w", err)
	}

	if !mv.IsFolder {
		return false, nil
	}

	return m.isParentOf(id, parentID, true)
}

func (m *movies) IsParentFolder(id, parentID string) (bool, error) {
	if parentID == "" {
		return id != "", nil
	}

	mv, err := m.GetMovieByID(parentID)
	if err != nil {
		return false, fmt.Errorf("get parent movie failed: %w", err)
	}

	firstCheck := true
	if mv.IsFolder {
		firstCheck = false
	} else {
		parentID = mv.ParentID.String()
	}

	return m.isParentOf(id, parentID, firstCheck)
}

func (m *movies) isParentOf(id, parentID string, firstCheck bool) (bool, error) {
	mv, err := m.GetMovieByID(id)
	if err != nil {
		return false, err
	}

	if mv.ParentID == "" {
		return false, nil
	}

	if mv.ParentID == model.EmptyNullString(parentID) {
		return !firstCheck, nil
	}

	return m.isParentOf(string(mv.ParentID), parentID, false)
}
