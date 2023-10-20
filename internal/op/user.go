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

func (u *User) HasPermission(room *Room, permission model.Permission) bool {
	return room.HasPermission(&u.User, permission)
}

func (u *User) DeleteRoom(room *Room) error {
	if !u.HasPermission(room, model.CanDeleteRoom) {
		return errors.New("no permission")
	}
	return DeleteRoom(room)
}
