package op

import (
	"context"
	"errors"
	"fmt"
	"sync/atomic"

	"github.com/gorilla/websocket"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type Room struct {
	current *current
	hub     atomic.Pointer[Hub]
	movies  *movies
	members rwmap.RWMap[string, *model.RoomMember]
	model.Room
}

func (r *Room) lazyInitHub() *Hub {
	h := r.hub.Load()
	if h == nil {
		h = newHub(r.ID)
		if !r.hub.CompareAndSwap(nil, h) {
			h.Close()
			return r.lazyInitHub()
		}
	}
	return h
}

func (r *Room) HubIsNotInited() bool {
	return r.hub.Load() == nil
}

func (r *Room) ViewerCount() int64 {
	if r.HubIsNotInited() {
		return 0
	}
	return r.lazyInitHub().ClientNum()
}

func (r *Room) KickUser(userID string) error {
	if r.HubIsNotInited() {
		return nil
	}
	return r.lazyInitHub().KickUser(userID)
}

func (r *Room) Broadcast(data Message, conf ...BroadcastConf) error {
	if r.HubIsNotInited() {
		return nil
	}
	return r.lazyInitHub().Broadcast(data, conf...)
}

func (r *Room) SendToUser(user *User, data Message) error {
	return r.SendToUserWithID(user.ID, data)
}

func (r *Room) SendToUserWithID(userID string, data Message) error {
	if r.HubIsNotInited() {
		return nil
	}
	return r.lazyInitHub().SendToUser(userID, data)
}

func (r *Room) SendToConnID(userID, connID string, data Message) error {
	if r.HubIsNotInited() {
		return nil
	}
	return r.lazyInitHub().SendToConnID(userID, connID, data)
}

func (r *Room) GetChannel(channelName string) (*rtmps.Channel, error) {
	return r.movies.GetChannel(channelName)
}

func (r *Room) close() {
	if h := r.hub.Load(); h != nil {
		if r.hub.CompareAndSwap(h, nil) {
			h.Close()
		}
	}
	r.movies.Close()
	r.members.Clear()
}

func (r *Room) UpdateMovie(movieID string, movie *model.MovieBase) error {
	err := r.checkCanModifyMovie(movieID)
	if err != nil {
		return err
	}
	return r.movies.Update(movieID, movie)
}

func (r *Room) AddMovie(m *model.Movie) error {
	m.RoomID = r.ID
	return r.movies.AddMovie(m)
}

func (r *Room) AddMovies(movies []*model.Movie) error {
	for _, m := range movies {
		m.RoomID = r.ID
	}
	return r.movies.AddMovies(movies)
}

func (r *Room) UserRole(userID string) (model.RoomMemberRole, error) {
	if r.IsCreator(userID) {
		return model.RoomMemberRoleCreator, nil
	}
	rur, err := r.LoadMember(userID)
	if err != nil {
		return model.RoomMemberRoleUnknown, err
	}
	return rur.Role, nil
}

// do not use this value for permission determination
func (r *Room) IsAdmin(userID string) bool {
	role, err := r.UserRole(userID)
	if err != nil {
		return false
	}
	return role.IsAdmin()
}

func (r *Room) IsCreator(userID string) bool {
	return r.CreatorID == userID
}

func (r *Room) IsGuest(userID string) bool {
	return userID == db.GuestUserID
}

func (r *Room) HasPermission(userID string, permission model.RoomMemberPermission) bool {
	if r.IsCreator(userID) {
		return true
	}

	rur, err := r.LoadOrCreateMember(userID)
	if err != nil {
		return false
	}

	if rur.Role.IsAdmin() {
		return true
	}

	switch {
	case permission.Has(model.PermissionGetMovieList) && !r.Settings.CanGetMovieList,
		permission.Has(model.PermissionAddMovie) && !r.Settings.CanAddMovie,
		permission.Has(model.PermissionDeleteMovie) && !r.Settings.CanDeleteMovie,
		permission.Has(model.PermissionEditMovie) && !r.Settings.CanEditMovie,
		permission.Has(model.PermissionSetCurrentMovie) && !r.Settings.CanSetCurrentMovie,
		permission.Has(model.PermissionSetCurrentStatus) && !r.Settings.CanSetCurrentStatus,
		permission.Has(model.PermissionSendChatMessage) && !r.Settings.CanSendChatMessage:
		return false
	default:
		return rur.Permissions.Has(permission)
	}
}

func (r *Room) HasAdminPermission(userID string, permission model.RoomAdminPermission) bool {
	if r.IsCreator(userID) {
		return true
	}

	rur, err := r.LoadOrCreateMember(userID)
	if err != nil {
		return false
	}

	return rur.HasAdminPermission(permission)
}

func (r *Room) LoadOrCreateMemberStatus(userID string) (model.RoomMemberStatus, error) {
	if r.IsCreator(userID) {
		return model.RoomMemberStatusActive, nil
	}
	rur, err := r.LoadOrCreateMember(userID)
	if err != nil {
		return model.RoomMemberStatusNotJoined, err
	}
	return rur.Status, nil
}

func (r *Room) LoadMemberStatus(userID string) (model.RoomMemberStatus, error) {
	if r.IsCreator(userID) {
		return model.RoomMemberStatusActive, nil
	}
	rur, err := r.LoadMember(userID)
	if err != nil {
		if errors.Is(err, db.NotFoundError(db.ErrRoomMemberNotFound)) {
			return model.RoomMemberStatusNotJoined, nil
		}
		return model.RoomMemberStatusNotJoined, err
	}
	return rur.Status, nil
}

func (r *Room) LoadOrCreateMember(userID string) (*model.RoomMember, error) {
	if r.Settings.DisableJoinNewUser {
		return r.LoadMember(userID)
	}
	if r.IsGuest(userID) && (r.Settings.DisableGuest || !settings.EnableGuest.Get()) {
		return nil, errors.New("guest is disabled")
	}
	member, ok := r.members.Load(userID)
	if ok {
		return member, nil
	}
	var conf []db.CreateRoomMemberRelationConfig
	if r.IsCreator(userID) {
		conf = append(
			conf,
			db.WithRoomMemberStatus(model.RoomMemberStatusActive),
			db.WithRoomMemberPermissions(model.AllPermissions),
			db.WithRoomMemberRole(model.RoomMemberRoleCreator),
			db.WithRoomMemberAdminPermissions(model.AllAdminPermissions),
		)
	} else {
		if r.IsGuest(userID) {
			conf = append(
				conf,
				db.WithRoomMemberPermissions(model.NoPermission),
				db.WithRoomMemberRole(model.RoomMemberRoleMember),
				db.WithRoomMemberAdminPermissions(model.NoAdminPermission),
			)
		} else {
			conf = append(
				conf,
				db.WithRoomMemberPermissions(r.Settings.UserDefaultPermissions),
				db.WithRoomMemberRole(model.RoomMemberRoleMember),
				db.WithRoomMemberAdminPermissions(model.NoAdminPermission),
			)
		}
		if r.Settings.JoinNeedReview {
			conf = append(conf, db.WithRoomMemberStatus(model.RoomMemberStatusPending))
		} else {
			conf = append(conf, db.WithRoomMemberStatus(model.RoomMemberStatusActive))
		}
	}
	member, err := db.FirstOrCreateRoomMemberRelation(r.ID, userID, conf...)
	if err != nil {
		return nil, err
	}
	return r.storeMember(userID, member), nil
}

func (r *Room) LoadMember(userID string) (*model.RoomMember, error) {
	if r.IsGuest(userID) && (r.Settings.DisableGuest || !settings.EnableGuest.Get()) {
		return nil, errors.New("guest is disabled")
	}
	member, ok := r.members.Load(userID)
	if ok {
		return member, nil
	}
	member, err := db.GetRoomMember(r.ID, userID)
	if err != nil {
		return nil, fmt.Errorf("get room member failed: %w", err)
	}
	return r.storeMember(userID, member), nil
}

func (r *Room) storeMember(userID string, member *model.RoomMember) *model.RoomMember {
	if r.IsCreator(userID) {
		member.Role = model.RoomMemberRoleCreator
		member.Permissions = model.AllPermissions
		member.AdminPermissions = model.AllAdminPermissions
		member.Status = model.RoomMemberStatusActive
	} else if r.IsGuest(userID) {
		member.Role = model.RoomMemberRoleMember
		member.Permissions = r.Settings.GuestPermissions
		member.AdminPermissions = model.NoAdminPermission
		if member.Status.IsBanned() {
			member.Status = model.RoomMemberStatusActive
		}
	} else if member.Role.IsAdmin() {
		member.Permissions = model.AllPermissions
	}
	member, _ = r.members.LoadOrStore(userID, member)
	return member
}

func (r *Room) LoadRoomMemberPermission(userID string) (model.RoomMemberPermission, error) {
	if r.IsCreator(userID) {
		return model.AllPermissions, nil
	}
	member, err := r.LoadMember(userID)
	if err != nil {
		return model.NoPermission, err
	}
	return member.Permissions, nil
}

func (r *Room) LoadRoomAdminPermission(userID string) (model.RoomAdminPermission, error) {
	if r.IsCreator(userID) {
		return model.AllAdminPermissions, nil
	}
	member, err := r.LoadMember(userID)
	if err != nil {
		return model.NoAdminPermission, err
	}
	return member.AdminPermissions, nil
}

func (r *Room) NeedPassword() bool {
	return len(r.HashedPassword) != 0
}

func (r *Room) SetPassword(password string) error {
	if r.CheckPassword(password) && r.NeedPassword() {
		return errors.New("password is the same")
	}
	var hashedPassword []byte
	if password != "" {
		var err error
		hashedPassword, err = bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
	}
	r.HashedPassword = hashedPassword
	return db.SetRoomHashedPassword(r.ID, hashedPassword)
}

func (r *Room) checkCanModifyMovie(id string) error {
	if id == "" {
		if r.current.current.Movie.ID != "" {
			return errors.New("cannot modify current movie")
		}
		return nil
	}
	cid := r.current.current.Movie.ID
	if cid != "" {
		if cid == id {
			return errors.New("cannot modify current movie")
		}
		ok, err := r.movies.IsParentFolder(cid, id)
		if err != nil {
			return fmt.Errorf("check parent failed: %w", err)
		}
		if ok {
			return errors.New("cannot modify current movie's parent")
		}
	}
	return nil
}

func (r *Room) checkCanModifyMovies(ids []string) error {
	if len(ids) == 0 {
		return errors.New("ids is nil")
	}
	cid := r.current.current.Movie.ID
	for _, id := range ids {
		if id == "" {
			if cid != "" {
				return errors.New("cannot modify current movie")
			}
		}
		if cid != "" {
			if id == cid {
				return errors.New("cannot modify current movie")
			}
			ok, err := r.movies.IsParentFolder(cid, id)
			if err != nil {
				return fmt.Errorf("check parent failed: %w", err)
			}
			if ok {
				return errors.New("cannot modify current movie's parent")
			}
		}
	}
	return nil
}

func (r *Room) DeleteMovieByID(id string) error {
	err := r.checkCanModifyMovie(id)
	if err != nil {
		return err
	}
	return r.movies.DeleteMovieByID(id)
}

func (r *Room) DeleteMoviesByID(ids []string) error {
	err := r.checkCanModifyMovies(ids)
	if err != nil {
		return err
	}
	return r.movies.DeleteMoviesByID(ids)
}

func (r *Room) ClearMovies() error {
	return r.ClearMoviesByParentID("")
}

func (r *Room) ClearMoviesByParentID(parentID string) error {
	err := r.checkCanModifyMovie(parentID)
	if err != nil {
		return err
	}
	return r.movies.DeleteMovieByParentID(parentID)
}

func (r *Room) GetMovieByID(id string) (*Movie, error) {
	return r.movies.GetMovieByID(id)
}

func (r *Room) Current() *Current {
	c := r.current.Current()
	return &c
}

func (r *Room) CurrentMovie() CurrentMovie {
	return r.current.current.Movie
}

var ErrNoCurrentMovie = errors.New("no current movie")

func (r *Room) LoadCurrentMovie() (*Movie, error) {
	id := r.current.current.Movie.ID
	if id == "" {
		return nil, ErrNoCurrentMovie
	}
	return r.GetMovieByID(id)
}

func (r *Room) CheckCurrentExpired(ctx context.Context, expireID uint64) (bool, error) {
	m, err := r.LoadCurrentMovie()
	if err != nil {
		return false, err
	}
	return m.CheckExpired(ctx, expireID)
}

func (r *Room) SetCurrentMovie(movieID string, subPath string, play bool) error {
	currentMovie, err := r.LoadCurrentMovie()
	if err != nil {
		if !errors.Is(err, ErrNoCurrentMovie) {
			return err
		}
	} else {
		if currentMovie.Proxy {
			err = currentMovie.Close()
		} else {
			err = currentMovie.ClearCache()
		}
		if err != nil {
			logrus.Errorf("clear current movie cache failed: %v", err)
		}
	}
	if movieID == "" {
		r.current.SetMovie(CurrentMovie{}, false)
		return nil
	}
	m, err := r.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	if m.IsFolder && !m.IsDynamicFolder() {
		return errors.New("cannot set static folder as current movie")
	}
	m.subPath = subPath
	r.current.SetMovie(CurrentMovie{
		ID:     m.ID,
		IsLive: m.Live,
	}, play)
	return m.ClearCache()
}

func (r *Room) SwapMoviePositions(id1, id2 string) error {
	return r.movies.SwapMoviePositions(id1, id2)
}

func (r *Room) GetMoviesWithPage(keyword string, page, pageSize int, parentID string) ([]*model.Movie, int64, error) {
	return r.movies.GetMoviesWithPage(keyword, page, pageSize, parentID)
}

func (r *Room) NewClient(user *User, conn *websocket.Conn) (*Client, error) {
	h := r.lazyInitHub()
	cli := newClient(user, r, h, conn)
	err := h.RegClient(cli)
	if err != nil {
		return nil, err
	}
	return cli, nil
}

func (r *Room) RegClient(cli *Client) error {
	return r.lazyInitHub().RegClient(cli)
}

func (r *Room) UnregisterClient(cli *Client) error {
	return r.lazyInitHub().UnRegClient(cli)
}

func (r *Room) UserIsOnline(userID string) bool {
	return r.lazyInitHub().IsOnline(userID)
}

func (r *Room) UserOnlineCount(userID string) int {
	return r.lazyInitHub().OnlineCount(userID)
}

func (r *Room) SetCurrentStatus(playing bool, seek float64, rate float64, timeDiff float64) *Status {
	return r.current.SetStatus(playing, seek, rate, timeDiff)
}

func (r *Room) SetCurrentSeekRate(seek float64, rate float64, timeDiff float64) *Status {
	return r.current.SetSeekRate(seek, rate, timeDiff)
}

func (r *Room) SetSettings(settings *model.RoomSettings) error {
	err := db.SaveRoomSettings(r.ID, settings)
	if err != nil {
		return err
	}
	return r.afterUpdateSettings(settings)
}

func (r *Room) UpdateSettings(settings map[string]any) error {
	rs, err := db.UpdateRoomSettings(r.ID, settings)
	if err != nil {
		return err
	}
	return r.afterUpdateSettings(rs)
}

func (r *Room) afterUpdateSettings(rs *model.RoomSettings) error {
	if r.Settings.GuestPermissions != rs.GuestPermissions {
		r.members.Delete(db.GuestUserID)
	}
	r.Settings = rs
	if rs.DisableGuest {
		return r.KickUser(db.GuestUserID)
	}
	return nil
}

func (r *Room) ResetMemberPermissions(userID string) error {
	return r.SetMemberPermissions(userID, r.Settings.UserDefaultPermissions)
}

func (r *Room) SetGuestPermissions(permissions model.RoomMemberPermission) error {
	return r.UpdateSettings(map[string]any{
		"guest_permissions": permissions,
	})
}

func (r *Room) SetMemberPermissions(userID string, permissions model.RoomMemberPermission) error {
	if r.IsCreator(userID) {
		return errors.New("you are creator, cannot set permissions")
	}
	if r.IsGuest(userID) {
		return r.SetGuestPermissions(permissions)
	}
	defer r.members.Delete(userID)
	return db.SetMemberPermissions(r.ID, userID, permissions)
}

func (r *Room) AddMemberPermissions(userID string, permissions model.RoomMemberPermission) error {
	if r.IsGuest(userID) {
		return r.SetGuestPermissions(r.Settings.GuestPermissions.Add(permissions))
	}
	if r.IsAdmin(userID) {
		return errors.New("cannot add permissions to admin")
	}
	defer r.members.Delete(userID)
	return db.AddMemberPermissions(r.ID, userID, permissions)
}

func (r *Room) RemoveMemberPermissions(userID string, permissions model.RoomMemberPermission) error {
	if r.IsGuest(userID) {
		return r.SetGuestPermissions(r.Settings.GuestPermissions.Remove(permissions))
	}
	if r.IsAdmin(userID) {
		return errors.New("cannot remove permissions from admin")
	}
	defer r.members.Delete(userID)
	return db.RemoveMemberPermissions(r.ID, userID, permissions)
}

func (r *Room) ApprovePendingMember(userID string) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot be approved as a pending member")
	}
	defer r.members.Delete(userID)
	return db.RoomApprovePendingMember(r.ID, userID)
}

func (r *Room) BanMember(userID string) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot be banned")
	}
	if r.IsGuest(userID) {
		return errors.New("please set whether to disable guest users in the room settings")
	}
	defer func() {
		r.members.Delete(userID)
		_ = r.KickUser(userID)
	}()
	return db.RoomBanMember(r.ID, userID)
}

func (r *Room) UnbanMember(userID string) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot be unbanned")
	}
	if r.IsGuest(userID) {
		return errors.New("please set whether to enable guest users in the room settings")
	}
	defer r.members.Delete(userID)
	return db.RoomUnbanMember(r.ID, userID)
}

func (r *Room) DeleteMember(userID string) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot be deleted")
	}
	defer func() {
		r.members.Delete(userID)
		_ = r.KickUser(userID)
	}()
	return db.DeleteRoomMember(r.ID, userID)
}

func (r *Room) ResetAdminPermissions(userID string) error {
	return r.SetAdminPermissions(userID, model.DefaultAdminPermissions)
}

func (r *Room) SetAdminPermissions(userID string, permissions model.RoomAdminPermission) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot set admin permissions")
	}
	if r.IsGuest(userID) {
		return errors.New("cannot set admin permissions to guest")
	}
	if member, err := r.LoadMember(userID); err != nil {
		return err
	} else if !member.Role.IsAdmin() {
		return errors.New("not admin")
	}
	defer r.members.Delete(userID)
	return db.RoomSetAdminPermissions(r.ID, userID, permissions)
}

func (r *Room) AddAdminPermissions(userID string, permissions model.RoomAdminPermission) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot add admin permissions")
	}
	if r.IsGuest(userID) {
		return errors.New("cannot add admin permissions to guest")
	}
	if member, err := r.LoadMember(userID); err != nil {
		return err
	} else if !member.Role.IsAdmin() {
		return errors.New("not admin")
	}
	defer r.members.Delete(userID)
	return db.RoomSetAdminPermissions(r.ID, userID, permissions)
}

func (r *Room) RemoveAdminPermissions(userID string, permissions model.RoomAdminPermission) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot remove admin permissions")
	}
	if r.IsGuest(userID) {
		return errors.New("cannot remove admin permissions from guest")
	}
	if member, err := r.LoadMember(userID); err != nil {
		return err
	} else if !member.Role.IsAdmin() {
		return errors.New("not admin")
	}
	defer r.members.Delete(userID)
	return db.RoomSetAdminPermissions(r.ID, userID, 0)
}

func (r *Room) SetAdmin(userID string, permissions model.RoomAdminPermission) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot set admin")
	}
	if r.IsGuest(userID) {
		return errors.New("cannot set guest as admin")
	}
	defer r.members.Delete(userID)
	return db.RoomSetAdmin(r.ID, userID, permissions)
}

func (r *Room) SetMember(userID string, permissions model.RoomMemberPermission) error {
	if r.IsCreator(userID) {
		return errors.New("creator cannot set member")
	}
	defer r.members.Delete(userID)
	return db.RoomSetMember(r.ID, userID, permissions)
}

func (r *Room) EnabledGuest() bool {
	return !r.Settings.DisableGuest && settings.EnableGuest.Get() && !r.NeedPassword()
}

func (r *Room) SetStatus(status model.RoomStatus) error {
	if err := db.SetRoomStatus(r.ID, status); err != nil {
		return err
	}
	r.Status = status
	if status == model.RoomStatusBanned || status == model.RoomStatusPending {
		r.close()
	}
	return nil
}
