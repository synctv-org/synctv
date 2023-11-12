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

func (u *User) CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*Room, error) {
	if u.IsBanned() {
		return nil, errors.New("user banned")
	}
	if u.IsAdmin() {
		conf = append(conf, db.WithStatus(model.RoomStatusActive))
	} else {
		if password == "" && settings.RoomMustNeedPwd.Get() {
			return nil, errors.New("room must need password")
		}
		if settings.CreateRoomNeedReview.Get() {
			conf = append(conf, db.WithStatus(model.RoomStatusPending))
		} else {
			conf = append(conf, db.WithStatus(model.RoomStatusActive))
		}
	}

	var maxCount int64
	if !u.IsAdmin() {
		maxCount = settings.UserMaxRoomCount.Get()
	}

	return CreateRoom(name, password, maxCount, append(conf, db.WithCreator(&u.User))...)
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
	if !u.IsAdmin() && password == "" && settings.RoomMustNeedPwd.Get() {
		return errors.New("room must need password")
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

func (u *User) DeleteMovieByID(room *Room, movieID string) error {
	m, err := room.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	if m.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
		return model.ErrNoPermission
	}
	return room.DeleteMovieByID(movieID)
}

func (u *User) DeleteMoviesByID(room *Room, movieIDs []string) error {
	for _, id := range movieIDs {
		m, err := room.GetMovieByID(id)
		if err != nil {
			return err
		}
		if m.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
			return model.ErrNoPermission
		}
	}
	for _, v := range movieIDs {
		if err := room.DeleteMovieByID(v); err != nil {
			return err
		}
	}
	return nil
}

func (u *User) ClearMovies(room *Room) error {
	if !u.HasRoomPermission(room, model.PermissionEditUser) {
		return model.ErrNoPermission
	}
	return room.ClearMovies()
}

func (u *User) SetCurrentMovie(room *Room, movie *model.Movie, play bool) error {
	if !u.HasRoomPermission(room, model.PermissionEditCurrent) {
		return model.ErrNoPermission
	}
	room.SetCurrentMovie(movie, play)
	return nil
}

func (u *User) SetCurrentMovieByID(room *Room, movieID string, play bool) error {
	m, err := room.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	return u.SetCurrentMovie(room, m.Movie, play)
}
