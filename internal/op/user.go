package op

import (
	"errors"
	"hash/crc32"
	"sync/atomic"

	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/email"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/settings"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type User struct {
	model.User
	version       uint32
	alistCache    atomic.Pointer[cache.AlistUserCache]
	bilibiliCache atomic.Pointer[cache.BilibiliUserCache]
	embyCache     atomic.Pointer[cache.EmbyUserCache]
}

func (u *User) AlistCache() *cache.AlistUserCache {
	c := u.alistCache.Load()
	if c == nil {
		c = cache.NewAlistUserCache(u.ID)
		if !u.alistCache.CompareAndSwap(nil, c) {
			return u.AlistCache()
		}
	}
	return c
}

func (u *User) BilibiliCache() *cache.BilibiliUserCache {
	c := u.bilibiliCache.Load()
	if c == nil {
		c = cache.NewBilibiliUserCache(u.ID)
		if !u.bilibiliCache.CompareAndSwap(nil, c) {
			return u.BilibiliCache()
		}
	}
	return c
}

func (u *User) EmbyCache() *cache.EmbyUserCache {
	c := u.embyCache.Load()
	if c == nil {
		c = cache.NewEmbyUserCache(u.ID)
		if !u.embyCache.CompareAndSwap(nil, c) {
			return u.EmbyCache()
		}
	}
	return c
}

func (u *User) Version() uint32 {
	return atomic.LoadUint32(&u.version)
}

func (u *User) CheckVersion(version uint32) bool {
	return atomic.LoadUint32(&u.version) == version
}

func (u *User) SetPassword(password string) error {
	if u.CheckPassword(password) {
		return errors.New("password is the same")
	}
	hashedPassword, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
	if err != nil {
		return err
	}
	atomic.StoreUint32(&u.version, crc32.ChecksumIEEE(hashedPassword))
	u.HashedPassword = hashedPassword
	return db.SetUserHashedPassword(u.ID, hashedPassword)
}

func (u *User) CreateRoom(name, password string, conf ...db.CreateRoomConfig) (*RoomEntry, error) {
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

func (u *User) NewMovie(movie *model.BaseMovie) (*model.Movie, error) {
	if movie == nil {
		return nil, errors.New("movie is nil")
	}
	switch movie.VendorInfo.Vendor {
	case model.VendorBilibili:
		if movie.VendorInfo.Bilibili == nil {
			return nil, errors.New("bilibili payload is nil")
		}
	case model.VendorAlist:
		if movie.VendorInfo.Alist == nil {
			return nil, errors.New("alist payload is nil")
		}
	}
	return &model.Movie{
		Base:      *movie,
		CreatorID: u.ID,
	}, nil
}

func (u *User) AddMovieToRoom(room *Room, movie *model.BaseMovie) error {
	if !u.HasRoomPermission(room, model.PermissionCreateMovie) {
		return model.ErrNoPermission
	}
	m, err := u.NewMovie(movie)
	if err != nil {
		return err
	}
	err = room.AddMovie(m)
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) NewMovies(movies []*model.BaseMovie) ([]*model.Movie, error) {
	var ms = make([]*model.Movie, len(movies))
	for i, m := range movies {
		movie, err := u.NewMovie(m)
		if err != nil {
			return nil, err
		}
		ms[i] = movie
	}
	return ms, nil
}

func (u *User) AddMoviesToRoom(room *Room, movies []*model.BaseMovie) error {
	if !u.HasRoomPermission(room, model.PermissionCreateMovie) {
		return model.ErrNoPermission
	}
	m, err := u.NewMovies(movies)
	if err != nil {
		return err
	}
	err = room.AddMovies(m)
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
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

func (u *User) DeleteRoom(room *RoomEntry) error {
	if !u.HasRoomPermission(room.Value(), model.PermissionEditRoom) {
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

func (u *User) UpdateMovie(room *Room, movieID string, movie *model.BaseMovie) error {
	m, err := room.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	if m.Movie.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
		return model.ErrNoPermission
	}
	err = room.UpdateMovie(movieID, movie)
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
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
	if m.Movie.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
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
		if m.Movie.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionEditUser) {
			return model.ErrNoPermission
		}
	}
	if err := room.DeleteMoviesByID(movieIDs); err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) ClearMovies(room *Room) error {
	if !u.HasRoomPermission(room, model.PermissionEditRoom) {
		return model.ErrNoPermission
	}
	err := room.ClearMovies()
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) SwapMoviePositions(room *Room, id1, id2 string) error {
	if !u.HasRoomPermission(room, model.PermissionEditRoom) {
		return model.ErrNoPermission
	}
	err := room.SwapMoviePositions(id1, id2)
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) SetCurrentMovie(room *Room, movieID string, play bool) error {
	if !u.HasRoomPermission(room, model.PermissionEditCurrent) {
		return model.ErrNoPermission
	}
	err := room.SetCurrentMovie(movieID, play)
	if err != nil {
		return err
	}
	return room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_CURRENT_CHANGED,
		CurrentChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) BindProvider(p provider.OAuth2Provider, pid string) error {
	err := db.BindProvider(u.ID, p, pid)
	if err != nil {
		return err
	}
	return nil
}

func (u *User) SendBindCaptchaEmail(e string) error {
	return email.SendBindCaptchaEmail(u.ID, e)
}

func (u *User) VerifyBindCaptchaEmail(e, captcha string) (bool, error) {
	return email.VerifyBindCaptchaEmail(u.ID, e, captcha)
}

func (u *User) BindEmail(e string) error {
	err := db.BindEmail(u.ID, e)
	if err != nil {
		return err
	}
	u.Email = e
	return nil
}

func (u *User) UnbindEmail() error {
	err := db.UnbindEmail(u.ID)
	if err != nil {
		return err
	}
	u.Email = ""
	return nil
}

var ErrEmailUnbound = errors.New("email unbound")

func (u *User) SendTestEmail() error {
	if u.Email == "" {
		return ErrEmailUnbound
	}

	return email.SendTestEmail(u.Username, u.Email)
}

func (u *User) SendRetrievePasswordCaptchaEmail(host string) error {
	if u.Email == "" {
		return ErrEmailUnbound
	}

	return email.SendRetrievePasswordCaptchaEmail(u.ID, u.Email, host)
}

func (u *User) VerifyRetrievePasswordCaptchaEmail(e, captcha string) (bool, error) {
	if u.Email != e {
		return false, errors.New("email has changed, please resend the captcha email")
	}
	return email.VerifyRetrievePasswordCaptchaEmail(u.ID, e, captcha)
}
