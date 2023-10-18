package db

import (
	"errors"
	"fmt"

	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

func CreateMovie(movie *model.Movie) error {
	return db.Create(movie).Error
}

func GetAllMoviesByRoomID(roomID uint) ([]*model.Movie, error) {
	movies := []*model.Movie{}
	err := db.Where("room_id = ?", roomID).Order("position ASC").Find(&movies).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return movies, errors.New("room not found")
	}
	return movies, err
}

func DeleteMovieByID(roomID, id uint) error {
	err := db.Unscoped().Where("room_id = ? AND id = ?", roomID, id).Delete(&model.Movie{}).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or movie not found")
	}
	return err
}

func LoadAndDeleteMovieByID(roomID, id uint, columns ...clause.Column) (*model.Movie, error) {
	movie := &model.Movie{}
	err := db.Unscoped().Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", roomID, id).Delete(movie).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return movie, errors.New("room or movie not found")
	}
	return movie, err
}

func DeleteMoviesByRoomID(roomID uint) error {
	err := db.Unscoped().Where("room_id = ?", roomID).Delete(&model.Movie{}).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room not found")
	}
	return err
}

func LoadAndDeleteMoviesByRoomID(roomID uint, columns ...clause.Column) ([]*model.Movie, error) {
	movies := []*model.Movie{}
	err := db.Unscoped().Clauses(clause.Returning{Columns: columns}).Where("room_id = ?", roomID).Delete(&movies).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return nil, errors.New("room not found")
	}
	return movies, err
}

func UpdateMovie(movie *model.Movie, columns ...clause.Column) error {
	err := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Updates(movie).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or movie not found")
	}
	return err
}

func SaveMovie(movie *model.Movie, columns ...clause.Column) error {
	err := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Save(movie).Error
	if err != nil && errors.Is(err, gorm.ErrRecordNotFound) {
		return errors.New("room or movie not found")
	}
	return err
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
		if errors.Is(err, gorm.ErrRecordNotFound) {
			err = fmt.Errorf("movie with id %d not found", movie1ID)
		}
		return
	}
	err = tx.Select("position").Where("room_id = ? AND id = ?", roomID, movie2ID).First(movie2).Error
	if err != nil {
		if errors.Is(err, gorm.ErrRecordNotFound) {
			err = fmt.Errorf("movie with id %d not found", movie2ID)
		}
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
