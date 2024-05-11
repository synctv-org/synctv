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
	if u.IsGuest() {
		return errors.New("guest cannot set password")
	}
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
	if u.IsAdmin() {
		conf = append(conf, db.WithStatus(model.RoomStatusActive))
	} else {
		if password == "" && settings.RoomMustNeedPwd.Get() {
			return nil, errors.New("room must need password")
		}
		if password != "" && settings.RoomMustNoNeedPwd.Get() {
			return nil, errors.New("room must no need password")
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

func (u *User) NewMovie(movie *model.MovieBase) (*model.Movie, error) {
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
		MovieBase: *movie,
		CreatorID: u.ID,
	}, nil
}

func (u *User) AddRoomMovie(room *Room, movie *model.MovieBase) (*model.Movie, error) {
	if !u.HasRoomPermission(room, model.PermissionAddMovie) {
		return nil, model.ErrNoPermission
	}
	m, err := u.NewMovie(movie)
	if err != nil {
		return nil, err
	}
	err = room.AddMovie(m)
	if err != nil {
		return nil, err
	}
	return m, room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: u.Username,
			Userid:   u.ID,
		},
	})
}

func (u *User) NewMovies(movies []*model.MovieBase) ([]*model.Movie, error) {
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

func (u *User) AddRoomMovies(room *Room, movies []*model.MovieBase) ([]*model.Movie, error) {
	if !u.HasRoomPermission(room, model.PermissionAddMovie) {
		return nil, model.ErrNoPermission
	}
	m, err := u.NewMovies(movies)
	if err != nil {
		return nil, err
	}
	err = room.AddMovies(m)
	if err != nil {
		return nil, err
	}
	return m, room.Broadcast(&pb.ElementMessage{
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

func (u *User) IsGuest() bool {
	return u.ID == db.GuestUserID
}

func (u *User) HasRoomPermission(room *Room, permission model.RoomMemberPermission) bool {
	if u.IsAdmin() {
		return true
	}
	return room.HasPermission(u.ID, permission)
}

func (u *User) HasRoomAdminPermission(room *Room, permission model.RoomAdminPermission) bool {
	if u.IsAdmin() {
		return true
	}
	if u.IsGuest() {
		return false
	}
	return room.HasAdminPermission(u.ID, permission)
}

func (u *User) IsRoomAdmin(room *Room) bool {
	return room.IsAdmin(u.ID)
}

func (u *User) IsRoomCreator(room *Room) bool {
	return room.IsCreator(u.ID)
}

func (u *User) DeleteRoom(room *RoomEntry) error {
	if !u.HasRoomAdminPermission(room.Value(), model.PermissionDeleteRoom) {
		return model.ErrNoPermission
	}
	return CompareAndDeleteRoom(room)
}

func (u *User) SetRoomPassword(room *Room, password string) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetRoomPassword) {
		return model.ErrNoPermission
	}
	if !u.IsAdmin() {
		if password == "" && settings.RoomMustNeedPwd.Get() {
			return errors.New("room must need password")
		}
		if password != "" && settings.RoomMustNoNeedPwd.Get() {
			return errors.New("room must no need password")
		}
	}
	return room.SetPassword(password)
}

func (u *User) SetUserRole() error {
	if u.IsGuest() {
		return errors.New("cannot set guest role")
	}
	if err := db.SetUserRoleByID(u.ID); err != nil {
		return err
	}
	u.Role = model.RoleUser
	return nil
}

func (u *User) SetAdminRole() error {
	if u.IsGuest() {
		return errors.New("guest cannot be admin")
	}
	if err := db.SetAdminRoleByID(u.ID); err != nil {
		return err
	}
	u.Role = model.RoleAdmin
	return nil
}

func (u *User) SetRootRole() error {
	if u.IsGuest() {
		return errors.New("guest cannot be root")
	}
	if err := db.SetRootRoleByID(u.ID); err != nil {
		return err
	}
	u.Role = model.RoleRoot
	return nil
}

func (u *User) Ban() error {
	if u.IsGuest() {
		return errors.New("guest cannot be banned")
	}
	if err := db.BanUserByID(u.ID); err != nil {
		return err
	}
	u.Role = model.RoleBanned
	return nil
}

func (u *User) Unban() error {
	if err := db.UnbanUserByID(u.ID); err != nil {
		return err
	}
	u.Role = model.RoleUser
	return nil
}

func (u *User) SetUsername(username string) error {
	if err := db.SetUsernameByID(u.ID, username); err != nil {
		return err
	}
	u.Username = username
	return nil
}

func (u *User) UpdateRoomMovie(room *Room, movieID string, movie *model.MovieBase) error {
	if !u.HasRoomPermission(room, model.PermissionEditMovie) {
		return model.ErrNoPermission
	}
	err := room.UpdateMovie(movieID, movie)
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

func (u *User) SetRoomSettings(room *Room, setting *model.RoomSettings) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetRoomSettings) {
		return model.ErrNoPermission
	}
	return room.SetSettings(setting)
}

func (u *User) UpdateRoomSettings(room *Room, settings map[string]interface{}) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetRoomSettings) {
		return model.ErrNoPermission
	}
	return room.UpdateSettings(settings)
}

func (u *User) DeleteRoomMovieByID(room *Room, movieID string) error {
	m, err := room.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	if m.Movie.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionDeleteMovie) {
		return model.ErrNoPermission
	}
	return room.DeleteMovieByID(movieID)
}

func (u *User) DeleteRoomMoviesByID(room *Room, movieIDs []string) error {
	for _, id := range movieIDs {
		m, err := room.GetMovieByID(id)
		if err != nil {
			return err
		}
		if m.Movie.CreatorID != u.ID && !u.HasRoomPermission(room, model.PermissionDeleteMovie) {
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

func (u *User) ClearRoomMovies(room *Room) error {
	if !u.HasRoomPermission(room, model.PermissionDeleteMovie) {
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

func (u *User) SwapRoomMoviePositions(room *Room, id1, id2 string) error {
	if !u.HasRoomPermission(room, model.PermissionEditMovie) {
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

func (u *User) SetRoomCurrentMovie(room *Room, movieID string, subPath string, play bool) error {
	if !u.HasRoomPermission(room, model.PermissionSetCurrentMovie) {
		return model.ErrNoPermission
	}
	err := room.SetCurrentMovie(movieID, subPath, play)
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
	u.Email = model.EmptyNullString(e)
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

	return email.SendTestEmail(u.Username, u.Email.String())
}

func (u *User) SendRetrievePasswordCaptchaEmail(host string) error {
	if u.Email == "" {
		return ErrEmailUnbound
	}

	return email.SendRetrievePasswordCaptchaEmail(u.ID, u.Email.String(), host)
}

func (u *User) VerifyRetrievePasswordCaptchaEmail(e, captcha string) (bool, error) {
	if u.Email.String() != e {
		return false, errors.New("email has changed, please resend the captcha email")
	}
	return email.VerifyRetrievePasswordCaptchaEmail(u.ID, e, captcha)
}

func (u *User) GetRoomMoviesWithPage(room *Room, page, pageSize int, parentID string) ([]*model.Movie, int64, error) {
	if !u.HasRoomPermission(room, model.PermissionGetMovieList) {
		return nil, 0, model.ErrNoPermission
	}
	return room.GetMoviesWithPage(page, pageSize, parentID)
}

func (u *User) SetRoomCurrentSeekRate(room *Room, seek, rate, timeDiff float64) (*Status, error) {
	if !u.HasRoomPermission(room, model.PermissionSetCurrentStatus) {
		return nil, model.ErrNoPermission
	}
	return room.SetCurrentSeekRate(seek, rate, timeDiff), nil
}

func (u *User) SetRoomCurrentStatus(room *Room, playing bool, seek, rate, timeDiff float64) (*Status, error) {
	if !u.HasRoomPermission(room, model.PermissionSetCurrentStatus) {
		return nil, model.ErrNoPermission
	}
	return room.SetCurrentStatus(playing, seek, rate, timeDiff), nil
}

func (u *User) BanRoomMember(room *Room, userID string) error {
	if !u.HasRoomAdminPermission(room, model.PermissionBanRoomMember) {
		return model.ErrNoPermission
	}
	if u.ID == userID {
		return errors.New("cannot ban yourself")
	}
	if room.IsAdmin(userID) && !u.IsRoomCreator(room) {
		return errors.New("cannot ban admin")
	}
	return room.BanMember(userID)
}

func (u *User) UnbanRoomMember(room *Room, userID string) error {
	if !u.HasRoomAdminPermission(room, model.PermissionBanRoomMember) {
		return model.ErrNoPermission
	}
	if u.ID == userID {
		return errors.New("cannot unban yourself")
	}
	return room.UnbanMember(userID)
}

func (u *User) SetMemberPermissions(room *Room, userID string, permissions model.RoomMemberPermission) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetUserPermission) {
		return model.ErrNoPermission
	}
	if room.IsAdmin(userID) && !u.IsRoomCreator(room) {
		return errors.New("cannot set admin permissions")
	}
	return room.SetMemberPermissions(userID, permissions)
}

func (u *User) AddMemberPermissions(room *Room, userID string, permissions model.RoomMemberPermission) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetUserPermission) {
		return model.ErrNoPermission
	}
	if room.IsAdmin(userID) && !u.IsRoomCreator(room) {
		return errors.New("cannot add admin permissions")
	}
	return room.AddMemberPermissions(userID, permissions)
}

func (u *User) RemoveMemberPermissions(room *Room, userID string, permissions model.RoomMemberPermission) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetUserPermission) {
		return model.ErrNoPermission
	}
	if room.IsAdmin(userID) && !u.IsRoomCreator(room) {
		return errors.New("cannot remove admin permissions")
	}
	return room.RemoveMemberPermissions(userID, permissions)
}

func (u *User) ResetMemberPermissions(room *Room, userID string) error {
	if !u.HasRoomAdminPermission(room, model.PermissionSetUserPermission) {
		return model.ErrNoPermission
	}
	if room.IsAdmin(userID) && !u.IsRoomCreator(room) {
		return errors.New("cannot reset admin permissions")
	}
	return room.ResetMemberPermissions(userID)
}

func (u *User) ApproveRoomPendingMember(room *Room, userID string) error {
	if !u.HasRoomAdminPermission(room, model.PermissionApprovePendingMember) {
		return model.ErrNoPermission
	}
	return room.ApprovePendingMember(userID)
}

func (u *User) SetRoomAdmin(room *Room, userID string, permissions model.RoomAdminPermission) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.SetAdmin(userID, permissions)
}

func (u *User) SetRoomMember(room *Room, userID string, permissions model.RoomMemberPermission) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.SetMember(userID, permissions)
}

func (u *User) SetRoomAdminPermissions(room *Room, userID string, permissions model.RoomAdminPermission) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.SetAdminPermissions(userID, permissions)
}

func (u *User) AddRoomAdminPermissions(room *Room, userID string, permissions model.RoomAdminPermission) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.AddAdminPermissions(userID, permissions)
}

func (u *User) RemoveRoomAdminPermissions(room *Room, userID string, permissions model.RoomAdminPermission) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.RemoveAdminPermissions(userID, permissions)
}

func (u *User) ResetRoomAdminPermissions(room *Room, userID string) error {
	if !u.IsRoomCreator(room) {
		return model.ErrNoPermission
	}
	return room.ResetAdminPermissions(userID)
}
