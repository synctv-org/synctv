package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm/clause"
)

func CreateMovie(movie *model.Movie) error {
	return db.Create(movie).Error
}

func GetAllMoviesByRoomID(roomID uint) ([]model.Movie, error) {
	movies := []model.Movie{}
	err := db.Where("room_id = ?", roomID).Order("position ASC").Find(&movies).Error
	return movies, err
}

func DeleteMovieByID(roomID, id uint) error {
	return db.Where("room_id = ? AND id = ?", roomID, id).Delete(&model.Movie{}).Error
}

// TODO: delete error
func LoadAndDeleteMovieByID(roomID, id uint, columns ...clause.Column) (*model.Movie, error) {
	movie := &model.Movie{}
	err := db.Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", roomID, id).Delete(movie).Error
	return movie, err
}

func DeleteMoviesByRoomID(roomID uint) error {
	return db.Where("room_id = ?", roomID).Delete(&model.Movie{}).Error
}

func LoadAndDeleteMoviesByRoomID(roomID uint, columns ...clause.Column) ([]model.Movie, error) {
	movies := []model.Movie{}
	err := db.Clauses(clause.Returning{Columns: columns}).Where("room_id = ?", roomID).Delete(&movies).Error
	return movies, err
}

func UpdateMovie(movie model.Movie) error {
	return db.Model(&model.Movie{}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Updates(movie).Error
}

func LoadAndUpdateMovie(movie model.Movie, columns ...clause.Column) (*model.Movie, error) {
	err := db.Model(&model.Movie{}).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Updates(&movie).Error
	return &movie, err
}

func SwapMoviePositions(roomID uint, movie1ID uint, movie2ID uint) (err error) {
	tx := db.Begin()
	defer func() {
		if err != nil {
			tx.Rollback()
		} else {
			tx.Commit()
		}
	}()
	movie1 := &model.Movie{}
	movie2 := &model.Movie{}
	err = tx.Select("position").Where("room_id = ? AND id = ?", roomID, movie1ID).First(movie1).Error
	if err != nil {
		return
	}
	err = tx.Select("position").Where("room_id = ? AND id = ?", roomID, movie2ID).First(movie2).Error
	if err != nil {
		return
	}
	err = tx.Model(&model.Movie{}).Where("room_id = ? AND id = ?", roomID, movie1ID).Update("position", movie2.Position).Error
	if err != nil {
		return
	}
	err = tx.Model(&model.Movie{}).Where("room_id = ? AND id = ?", roomID, movie2ID).Update("position", movie1.Position).Error
	if err != nil {
		return
	}
	return
}
