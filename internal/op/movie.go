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

func GetMoviesByRoomID(roomID uint) ([]model.Movie, error) {
	i, err := movieCache.Get(roomID)
	if err == nil {
		return i.([]model.Movie), nil
	}
	m, err := db.GetMoviesByRoomID(roomID)
	if err != nil {
		return nil, err
	}
	return m, movieCache.SetWithExpire(roomID, m, time.Hour)
}

func GetMovieByID(roomID, id uint) (*model.Movie, error) {
	var m []model.Movie
	i, err := movieCache.Get(roomID)
	if err == nil {
		m = i.([]model.Movie)
	} else {
		m, err = GetMoviesByRoomID(roomID)
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
