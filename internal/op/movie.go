package op

import (
	"errors"
	"time"

	"github.com/bluele/gcache"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/zijiren233/gencontainer/dllist"
)

var movieCache = gcache.New(2048).
	LRU().
	Build()

func GetAllMoviesByRoomID(roomID uint) (*dllist.Dllist[*model.Movie], error) {
	i, err := movieCache.Get(roomID)
	if err == nil {
		return i.(*dllist.Dllist[*model.Movie]), nil
	}
	m, err := db.GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	d := dllist.New[*model.Movie]()
	for i := range m {
		d.PushBack(m[i])
	}
	return d, movieCache.SetWithExpire(roomID, d, time.Hour)
}

func GetMoviesByRoomIDWithPage(roomID uint, page, max int) ([]*model.Movie, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	start := (page - 1) * max
	if start >= ms.Len() {
		start = ms.Len()
	}
	end := start + max
	if end > ms.Len() {
		end = ms.Len()
	}
	var m []*model.Movie = make([]*model.Movie, 0, end-start)
	idx := 0
	for i := ms.Front(); i != nil; i = i.Next() {
		if idx >= start && idx < end {
			m = append(m, i.Value)
		}
		idx++
	}
	return m, nil
}

func GetMovieByID(roomID, id uint) (*model.Movie, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	for i := ms.Front(); i != nil; i = i.Next() {
		if i.Value.ID == id {
			return i.Value, nil
		}
	}
	return nil, errors.New("movie not found")
}

func GetMoviesCountByRoomID(roomID uint) (int, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return 0, err
	}
	return ms.Len(), nil
}

func DeleteMovieByID(roomID, id uint) error {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return err
	}
	for i := ms.Front(); i != nil; i = i.Next() {
		if i.Value.ID == id {
			ms.Remove(i)
			return db.DeleteMovieByID(roomID, id)
		}
	}
	return errors.New("movie not found")
}

func UpdateMovie(movie *model.Movie) error {
	err := db.UpdateMovie(movie)
	if err != nil {
		return err
	}
	m, err := GetMovieByID(movie.RoomID, movie.ID)
	if err != nil {
		return err
	}
	*m = *movie
	return nil
}

func SaveMovie(movie *model.Movie) error {
	log.Debug(movie)
	err := db.SaveMovie(movie)
	if err != nil {
		return err
	}
	m, err := GetMovieByID(movie.RoomID, movie.ID)
	if err != nil {
		return err
	}
	*m = *movie
	return nil
}

func DeleteMoviesByRoomID(roomID uint) error {
	movieCache.Remove(roomID)
	return db.DeleteMoviesByRoomID(roomID)
}

func LoadAndDeleteMovieByID(roomID, id uint) (*model.Movie, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	for i := ms.Front(); i != nil; i = i.Next() {
		if i.Value.ID == id {
			ms.Remove(i)
			return db.LoadAndDeleteMovieByID(roomID, id)
		}
	}
	return nil, errors.New("movie not found")
}

// data race
func CreateMovie(movie *model.Movie) error {
	ms, err := GetAllMoviesByRoomID(movie.RoomID)
	if err != nil {
		return err
	}
	err = db.CreateMovie(movie)
	if err != nil {
		return err
	}
	ms.PushBack(movie)
	return nil
}

func GetMovieWithPullKey(roomID uint, pullKey string) (*model.Movie, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	for i := ms.Front(); i != nil; i = i.Next() {
		if i.Value.PullKey == pullKey {
			return i.Value, nil
		}
	}
	return nil, errors.New("movie not found")
}

func SwapMoviePositions(roomID uint, movie1ID uint, movie2ID uint) error {
	ms, err := GetAllMoviesByRoomID(roomID)
	if err != nil {
		return err
	}
	var m1, m2 *model.Movie
	for i := ms.Front(); i != nil; i = i.Next() {
		if i.Value.ID == movie1ID {
			m1 = i.Value
		}
		if i.Value.ID == movie2ID {
			m2 = i.Value
		}
	}
	if m1 == nil || m2 == nil {
		return errors.New("movie not found")
	}
	m1.Position, m2.Position = m2.Position, m1.Position
	return db.SwapMoviePositions(roomID, movie1ID, movie2ID)
}
