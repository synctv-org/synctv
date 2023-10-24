package op

import (
	"errors"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type User struct {
	model.User
}

func (u *User) CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*model.Room, error) {
	return db.CreateRoom(name, password, append(conf, db.WithCreator(&u.User))...)
}

func (u *User) NewMovie(movie model.MovieInfo) model.Movie {
	return model.Movie{
		MovieInfo: movie,
		CreatorID: u.ID,
	}
}

func (u *User) HasPermission(roomID uint, permission model.Permission) bool {
	if u.Role == model.RoleAdmin {
		return true
	}
	ur, err := db.GetRoomUserRelation(roomID, u.ID)
	if err != nil {
		return false
	}
	return ur.HasPermission(permission)
}

func (u *User) DeleteRoom(roomID uint) error {
	if !u.HasPermission(roomID, model.CanDeleteRoom) {
		return errors.New("no permission")
	}
	return DeleteRoom(roomID)
}

func (u *User) SetRoomPassword(roomID uint, password string) error {
	if !u.HasPermission(roomID, model.CanSetRoomPassword) {
		return errors.New("no permission")
	}
	return SetRoomPassword(roomID, password)
}
