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

func (u *User) NewMovies(movies []*model.BaseMovie) []*model.Movie {
	var ms = make([]*model.Movie, 0, len(movies))
	for i, m := range movies {
		ms[i] = u.NewMovie(m)
	}
	return ms
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

func (u *User) HasPermission(roomID string, permission model.Permission) bool {
	if u.Role >= model.RoleAdmin {
		return true
	}
	ur, err := db.GetRoomUserRelation(roomID, u.ID)
	if err != nil {
		return false
	}
	return ur.HasPermission(permission)
}

func (u *User) DeleteRoom(roomID string) error {
	if !u.HasPermission(roomID, model.CanDeleteRoom) {
		return errors.New("no permission")
	}
	return DeleteRoom(roomID)
}

func (u *User) SetRoomPassword(roomID, password string) error {
	if !u.HasPermission(roomID, model.CanSetRoomPassword) {
		return errors.New("no permission")
	}
	return SetRoomPassword(roomID, password)
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
