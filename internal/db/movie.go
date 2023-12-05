package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

func CreateMovie(movie *model.Movie) error {
	return db.Create(movie).Error
}

func CreateMovies(movies []*model.Movie) error {
	return db.Create(movies).Error
}

func GetAllMoviesByRoomID(roomID string) []*model.Movie {
	movies := []*model.Movie{}
	db.Where("room_id = ?", roomID).Order("position ASC").Find(&movies)
	return movies
}

func DeleteMovieByID(roomID, id string) error {
	err := db.Unscoped().Where("room_id = ? AND id = ?", roomID, id).Delete(&model.Movie{}).Error
	return HandleNotFound(err, "room or movie")
}

func LoadAndDeleteMovieByID(roomID, id string, columns ...clause.Column) (*model.Movie, error) {
	movie := &model.Movie{}
	err := db.Unscoped().Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", roomID, id).Delete(movie).Error
	return movie, HandleNotFound(err, "room or movie")
}

func DeleteMoviesByRoomID(roomID string) error {
	err := db.Unscoped().Where("room_id = ?", roomID).Delete(&model.Movie{}).Error
	return HandleNotFound(err, "room")
}

func LoadAndDeleteMoviesByRoomID(roomID string, columns ...clause.Column) ([]*model.Movie, error) {
	movies := []*model.Movie{}
	err := db.Unscoped().Clauses(clause.Returning{Columns: columns}).Where("room_id = ?", roomID).Delete(&movies).Error
	return movies, HandleNotFound(err, "room")
}

func UpdateMovie(movie *model.Movie, columns ...clause.Column) error {
	err := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Updates(movie).Error
	return HandleNotFound(err, "room or movie")
}

func SaveMovie(movie *model.Movie, columns ...clause.Column) error {
	err := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Save(movie).Error
	return HandleNotFound(err, "room or movie")
}

func SwapMoviePositions(roomID, movie1ID, movie2ID string) (err error) {
	return Transactional(func(tx *gorm.DB) error {
		movie1 := &model.Movie{}
		movie2 := &model.Movie{}
		err = tx.Where("room_id = ? AND id = ?", roomID, movie1ID).First(movie1).Error
		if err != nil {
			return HandleNotFound(err, "movie1")
		}
		err = tx.Where("room_id = ? AND id = ?", roomID, movie2ID).First(movie2).Error
		if err != nil {
			return HandleNotFound(err, "movie2")
		}
		movie1.Position, movie2.Position = movie2.Position, movie1.Position
		err = tx.Save(movie1).Error
		if err != nil {
			return err
		}
		return tx.Save(movie2).Error
	})
}
