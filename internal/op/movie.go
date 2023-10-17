package op

import (
	"errors"
	"time"

	"github.com/bluele/gcache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

var movieCache = gcache.New(2048).
	LRU().
	Build()

func GetAllMoviesByRoomID(roomID uint) ([]model.Movie, error) {
	i, err := movieCache.Get(roomID)
	if err == nil {
		return i.([]model.Movie), nil
	}
	m, err := db.GetAllMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	return m, movieCache.SetWithExpire(roomID, m, time.Hour)
}

func GetMoviesByRoomIDWithPage(roomID uint, page, pageSize int) ([]model.Movie, error) {
	i, err := movieCache.Get(roomID)
	if err != nil {
		return nil, err
	}
	ms := i.([]model.Movie)
	if page < 1 {
		page = 1
	}
	if pageSize < 1 {
		pageSize = 1
	}
	start := (page - 1) * pageSize
	end := page * pageSize
	if start > len(ms) {
		start = len(ms)
	}
	if end > len(ms) {
		end = len(ms)
	}
	return ms[start:end], nil
}

func GetMovieByID(roomID, id uint) (*model.Movie, error) {
	var m []model.Movie
	i, err := movieCache.Get(roomID)
	if err == nil {
		m = i.([]model.Movie)
	} else {
		m, err = GetAllMoviesByRoomID(roomID)
		if err != nil {
			return nil, err
		}
	}
	for _, v := range m {
		if v.ID == id {
			return &v, nil
		}
	}
	return nil, errors.New("movie not found")
}

func GetMoviesCountByRoomID(roomID uint) (int, error) {
	ms, err := GetAllMoviesByRoomID(roomID)
	return len(ms), err
}

func DeleteMovieByID(roomID, id uint) error {
	err := db.DeleteMovieByID(roomID, id)
	if err != nil {
		return err
	}
	i, err := movieCache.Get(roomID)
	if err != nil {
		return err
	}
	ms := i.([]model.Movie)
	for i, v := range ms {
		if v.ID == id {
			ms = append(ms[:i], ms[i+1:]...)
			break
		}
	}
	return nil
}

func UpdateMovie(movie model.Movie) error {
	m, err := db.LoadAndUpdateMovie(movie)
	if err != nil {
		return err
	}
	i, err := movieCache.Get(movie.RoomID)
	if err != nil {
		return err
	}
	ms := i.([]model.Movie)
	for i, v := range ms {
		if v.ID == movie.ID {
			ms[i] = *m
			break
		}
	}
	return nil
}

func DeleteMoviesByRoomID(roomID uint) error {
	err := db.DeleteMoviesByRoomID(roomID)
	if err != nil {
		return err
	}
	movieCache.Remove(roomID)
	return nil
}

func LoadAndDeleteMovieByID(roomID, id uint) (*model.Movie, error) {
	m, err := db.LoadAndDeleteMovieByID(roomID, id)
	if err != nil {
		return nil, err
	}
	i, err := movieCache.Get(roomID)
	if err != nil {
		return nil, err
	}
	ms := i.([]model.Movie)
	for i, v := range ms {
		if v.ID == id {
			ms = append(ms[:i], ms[i+1:]...)
			break
		}
	}
	return m, nil
}

// data race
func CreateMovie(movie *model.Movie) error {
	err := db.CreateMovie(movie)
	if err != nil {
		return err
	}
	i, err := movieCache.Get(movie.RoomID)
	if err != nil {
		movieCache.Set(movie.RoomID, []model.Movie{*movie})
		return nil
	}
	ms := i.([]model.Movie)
	ms = append(ms, *movie)
	movieCache.Set(movie.RoomID, ms)
	return nil
}

func GetMovieWithPullKey(roomID uint, pullKey string) (*model.Movie, error) {
	i, err := movieCache.Get(roomID)
	if err != nil {
		return nil, err
	}
	ms := i.([]model.Movie)
	for _, v := range ms {
		if v.PullKey == pullKey {
			return &v, nil
		}
	}
	return nil, errors.New("movie not found")
}

func SwapMoviePositions(roomID uint, movie1ID uint, movie2ID uint) error {
	err := db.SwapMoviePositions(roomID, movie1ID, movie2ID)
	if err != nil {
		return err
	}
	i, err := movieCache.Get(roomID)
	if err != nil {
		return err
	}
	ms := i.([]model.Movie)
	movie1I, movie2I := 0, 0
	for i, v := range ms {
		if v.ID == movie1ID {
			movie1I = i
		}
		if v.ID == movie2ID {
			movie2I = i
		}
	}
	ms[movie1I].Position, ms[movie2I].Position = ms[movie2I].Position, ms[movie1I].Position
	ms[movie1I], ms[movie2I] = ms[movie2I], ms[movie1I]
	return nil
}
