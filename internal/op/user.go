package op

import (
	"errors"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
)

type User struct {
	model.User
}

func (u *User) CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*model.Room, error) {
	if u.IsBanned() {
		return nil, errors.New("user banned")
	}
	if !u.IsAdmin() && settings.CreateRoomNeedReview.Get() {
		conf = append(conf, db.WithStatus(model.RoomStatusPending))
	} else {
		conf = append(conf, db.WithStatus(model.RoomStatusActive))
	}
	return db.CreateRoom(name, password, append(conf, db.WithCreator(&u.User))...)
}

func (u *User) NewMovie(movie *model.BaseMovie) *model.Movie {
	return &model.Movie{
		Base:      *movie,
		CreatorID: u.ID,
	}
}

func (u *User) AddMovieToRoom(room *Room, movie *model.BaseMovie) error {
	if !u.HasRoomPermission(room, model.PermissionCreateMovie) {
		return model.ErrNoPermission
	}
	return room.AddMovie(u.NewMovie(movie))
}

func (u *User) NewMovies(movies []*model.BaseMovie) []*model.Movie {
	var ms = make([]*model.Movie, len(movies))
	for i, m := range movies {
		ms[i] = u.NewMovie(m)
	}
	return ms
}

func (u *User) AddMoviesToRoom(room *Room, movies []*model.BaseMovie) error {
	if !u.HasRoomPermission(room, model.PermissionCreateMovie) {
		return model.ErrNoPermission
	}
	return room.AddMovies(u.NewMovies(movies))
}

func (u *User) IsRoot() bool {
	return u.Role == model.RoleRoot
}

func (u *User) IsAdmin() bool {
	return u.Role == model.RoleAdmin || u.IsRoot()
}

func (u *User) IsBanned() bool {
	return u.Role == model.RoleBanned
}

func (u *User) IsPending() bool {
	return u.Role == model.RolePending
}

func (u *User) HasRoomPermission(room *Room, permission model.RoomUserPermission) bool {
	if u.IsAdmin() {
		return true
	}
	return room.HasPermission(u.ID, permission)
}

func (u *User) DeleteRoom(room *Room) error {
	if !u.HasRoomPermission(room, model.PermissionEditRoom) {
		return model.ErrNoPermission
	}
	return CompareAndDeleteRoom(room)
}

func (u *User) SetRoomPassword(room *Room, password string) error {
	if !u.HasRoomPermission(room, model.PermissionEditRoom) {
		return model.ErrNoPermission
	}
	return room.SetPassword(password)
}

func (u *User) SetRole(role model.Role) error {
	if err := db.SetRoleByID(u.ID, role); err != nil {
		return err
	}
	u.Role = role
	return nil
}

func (u *User) SetUsername(username string) error {
	if err := db.SetUsernameByID(u.ID, username); err != nil {
		return err
	}
	u.Username = username
	return nil
}

func (u *User) UpdateMovie(room *Room, movieID string, movie model.BaseMovie) error {
	m, err := room.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	if m.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
		return model.ErrNoPermission
	}
	return room.UpdateMovie(movieID, movie)
}

func (u *User) SetRoomSetting(room *Room, setting model.RoomSettings) error {
	if !u.HasRoomPermission(room, model.PermissionEditRoom) {
		return model.ErrNoPermission
	}
	return room.SetSettings(setting)
}
