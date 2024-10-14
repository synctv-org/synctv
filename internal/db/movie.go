package db

import (
	"github.com/synctv-org/synctv/internal/model"
	"gorm.io/gorm"
	"gorm.io/gorm/clause"
)

const (
	ErrRoomOrMovieNotFound = "room or movie"
)

func CreateMovie(movie *model.Movie) error {
	return db.Create(movie).Error
}

func CreateMovies(movies []*model.Movie) error {
	return db.CreateInBatches(movies, 100).Error
}

func WithParentMovieID(parentMovieID string) func(*gorm.DB) *gorm.DB {
	return func(db *gorm.DB) *gorm.DB {
		if parentMovieID == "" {
			return db.Where("base_parent_id IS NULL")
		}
		return db.Where("base_parent_id = ?", parentMovieID)
	}
}

func GetMoviesByRoomID(roomID string, scopes ...func(*gorm.DB) *gorm.DB) ([]*model.Movie, error) {
	var movies []*model.Movie
	err := db.Where("room_id = ?", roomID).Order("position ASC").Scopes(scopes...).Find(&movies).Error
	return movies, err
}

func GetMoviesCountByRoomID(roomID string, scopes ...func(*gorm.DB) *gorm.DB) (int64, error) {
	var count int64
	err := db.Model(&model.Movie{}).Where("room_id = ?", roomID).Scopes(scopes...).Count(&count).Error
	return count, err
}

func GetMovieByID(roomID, id string, scopes ...func(*gorm.DB) *gorm.DB) (*model.Movie, error) {
	var movie model.Movie
	err := db.Where("room_id = ? AND id = ?", roomID, id).Scopes(scopes...).First(&movie).Error
	return &movie, HandleNotFound(err, ErrRoomOrMovieNotFound)
}

func DeleteMovieByID(roomID, id string) error {
	result := db.Unscoped().Where("room_id = ? AND id = ?", roomID, id).Delete(&model.Movie{})
	return HandleUpdateResult(result, ErrRoomOrMovieNotFound)
}

func DeleteMoviesByID(roomID string, ids []string) error {
	result := db.Unscoped().Where("room_id = ? AND id IN ?", roomID, ids).Delete(&model.Movie{})
	return HandleUpdateResult(result, ErrRoomOrMovieNotFound)
}

func DeleteMoviesByRoomID(roomID string, scopes ...func(*gorm.DB) *gorm.DB) error {
	result := db.Where("room_id = ?", roomID).Scopes(scopes...).Delete(&model.Movie{})
	return HandleUpdateResult(result, ErrRoomOrMovieNotFound)
}

func DeleteMoviesByRoomIDAndParentID(roomID, parentID string) error {
	return DeleteMoviesByRoomID(roomID, WithParentMovieID(parentID))
}

func UpdateMovie(movie *model.Movie, columns ...clause.Column) error {
	result := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Updates(movie)
	return HandleUpdateResult(result, ErrRoomOrMovieNotFound)
}

func SaveMovie(movie *model.Movie, columns ...clause.Column) error {
	result := db.Model(movie).Clauses(clause.Returning{Columns: columns}).Where("room_id = ? AND id = ?", movie.RoomID, movie.ID).Omit("created_at").Save(movie)
	return HandleUpdateResult(result, ErrRoomOrMovieNotFound)
}

func SwapMoviePositions(roomID, movie1ID, movie2ID string) error {
	return db.Transaction(func(tx *gorm.DB) error {
		var movie1, movie2 model.Movie
		if err := tx.Where("room_id = ? AND id = ?", roomID, movie1ID).First(&movie1).Error; err != nil {
			return HandleNotFound(err, ErrRoomOrMovieNotFound)
		}
		if err := tx.Where("room_id = ? AND id = ?", roomID, movie2ID).First(&movie2).Error; err != nil {
			return HandleNotFound(err, ErrRoomOrMovieNotFound)
		}

		movie1.Position, movie2.Position = movie2.Position, movie1.Position

		result1 := tx.Model(&movie1).Where("room_id = ? AND id = ?", roomID, movie1ID).Update("position", movie1.Position)
		if err := HandleUpdateResult(result1, ErrRoomOrMovieNotFound); err != nil {
			return err
		}
		result2 := tx.Model(&movie2).Where("room_id = ? AND id = ?", roomID, movie2ID).Update("position", movie2.Position)
		return HandleUpdateResult(result2, ErrRoomOrMovieNotFound)
	})
}
