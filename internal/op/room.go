package op

import (
	"errors"
	"hash/crc32"
	"sync/atomic"

	"github.com/gorilla/websocket"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/utils"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type Room struct {
	model.Room
	version  uint32
	current  *current
	initOnce utils.Once
	hub      *Hub
	movies   movies
}

func (r *Room) lazyInitHub() {
	r.initOnce.Do(func() {
		r.hub = newHub(r.ID)
	})
}

func (r *Room) PeopleNum() int64 {
	if r.hub == nil {
		return 0
	}
	return r.hub.PeopleNum()
}

func (r *Room) Broadcast(data Message, conf ...BroadcastConf) error {
	if r.hub == nil {
		return nil
	}
	return r.hub.Broadcast(data, conf...)
}

func (r *Room) SendToUser(user *User, data Message) error {
	if r.hub == nil {
		return nil
	}
	return r.hub.SendToUser(user.ID, data)
}

func (r *Room) GetChannel(channelName string) (*rtmps.Channel, error) {
	return r.movies.GetChannel(channelName)
}

func (r *Room) close() {
	if r.initOnce.Done() {
		r.hub.Close()
		r.movies.Close()
	}
}

func (r *Room) Version() uint32 {
	return atomic.LoadUint32(&r.version)
}

func (r *Room) CheckVersion(version uint32) bool {
	return atomic.LoadUint32(&r.version) == version
}

func (r *Room) UpdateMovie(movieId string, movie *model.BaseMovie) error {
	if r.current.current.MovieID == movieId {
		return errors.New("cannot update current movie")
	}
	return r.movies.Update(movieId, movie)
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

func (r *Room) HasPermission(userID string, permission model.RoomUserPermission) bool {
	if r.CreatorID == userID {
		return true
	}

	rur, err := r.LoadOrCreateRoomUserRelation(userID)
	if err != nil {
		return false
	}

	return rur.HasPermission(permission)
}

func (r *Room) LoadOrCreateRoomUserRelation(userID string) (*model.RoomUserRelation, error) {
	var conf []db.CreateRoomUserRelationConfig
	if r.Settings.JoinNeedReview {
		conf = []db.CreateRoomUserRelationConfig{db.WithRoomUserRelationStatus(model.RoomUserStatusPending)}
	} else {
		conf = []db.CreateRoomUserRelationConfig{db.WithRoomUserRelationStatus(model.RoomUserStatusActive)}
	}
	if r.Settings.UserDefaultPermissions != 0 {
		conf = append(conf, db.WithRoomUserRelationPermissions(r.Settings.UserDefaultPermissions))
	}
	return db.FirstOrCreateRoomUserRelation(r.ID, userID, conf...)
}

func (r *Room) GetRoomUserRelation(userID string) (model.RoomUserPermission, error) {
	if r.CreatorID == userID {
		return model.PermissionAll, nil
	}
	ur, err := db.GetRoomUserRelation(r.ID, userID)
	if err != nil {
		return 0, err
	}
	return ur.Permissions, nil
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
		atomic.StoreUint32(&r.version, crc32.ChecksumIEEE(hashedPassword))
	}
	r.HashedPassword = hashedPassword
	return db.SetRoomHashedPassword(r.ID, hashedPassword)
}

func (r *Room) SetUserStatus(userID string, status model.RoomUserStatus) error {
	return db.SetRoomUserStatus(r.ID, userID, status)
}

func (r *Room) SetUserPermission(userID string, permission model.RoomUserPermission) error {
	return db.SetUserPermission(r.ID, userID, permission)
}

func (r *Room) AddUserPermission(userID string, permission model.RoomUserPermission) error {
	return db.AddUserPermission(r.ID, userID, permission)
}

func (r *Room) RemoveUserPermission(userID string, permission model.RoomUserPermission) error {
	return db.RemoveUserPermission(r.ID, userID, permission)
}

func (r *Room) GetMoviesCount() int {
	return r.movies.Len()
}

func (r *Room) DeleteMovieByID(id string) error {
	if r.current.current.MovieID == id {
		return errors.New("cannot delete current movie")
	}
	return r.movies.DeleteMovieByID(id)
}

func (r *Room) DeleteMoviesByID(ids []string) error {
	if r.current.current.MovieID != "" {
		for _, id := range ids {
			if id == r.current.current.MovieID {
				return errors.New("cannot delete current movie")
			}
		}
	}
	return r.movies.DeleteMoviesByID(ids)
}

func (r *Room) ClearMovies() error {
	if r.current.current.MovieID != "" {
		return errors.New("cannot clear movies when current movie is not empty")
	}
	return r.movies.Clear()
}

func (r *Room) GetMovieByID(id string) (*Movie, error) {
	return r.movies.GetMovieByID(id)
}

func (r *Room) Current() *Current {
	c := r.current.Current()
	return &c
}

var ErrNoCurrentMovie = errors.New("no current movie")

func (r *Room) CurrentMovie() (*Movie, error) {
	if r.current.current.MovieID == "" {
		return nil, ErrNoCurrentMovie
	}
	return r.GetMovieByID(r.current.current.MovieID)
}

func (r *Room) CheckCurrentExpired(expireId uint64) (bool, error) {
	m, err := r.CurrentMovie()
	if err != nil {
		return false, err
	}
	return m.CheckExpired(expireId), nil
}

func (r *Room) SetCurrentMovie(movieID string, play bool) error {
	if movieID == "" {
		r.current.SetMovie("", false, play)
		return nil
	}
	m, err := r.GetMovieByID(movieID)
	if err != nil {
		return err
	}
	r.current.SetMovie(m.ID, m.Base.Live, play)
	return nil
}

func (r *Room) SwapMoviePositions(id1, id2 string) error {
	return r.movies.SwapMoviePositions(id1, id2)
}

func (r *Room) GetMoviesWithPage(page, pageSize int) []*Movie {
	return r.movies.GetMoviesWithPage(page, pageSize)
}

func (r *Room) NewClient(user *User, conn *websocket.Conn) (*Client, error) {
	r.lazyInitHub()
	cli := newClient(user, r, conn)
	err := r.hub.RegClient(cli)
	if err != nil {
		return nil, err
	}
	return cli, nil
}

func (r *Room) RegClient(cli *Client) error {
	r.lazyInitHub()
	return r.hub.RegClient(cli)
}

func (r *Room) UnregisterClient(cli *Client) error {
	r.lazyInitHub()
	return r.hub.UnRegClient(cli)
}

func (r *Room) SetStatus(playing bool, seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetStatus(playing, seek, rate, timeDiff)
}

func (r *Room) SetSeekRate(seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetSeekRate(seek, rate, timeDiff)
}

func (r *Room) SetRoomStatus(status model.RoomStatus) error {
	err := db.SetRoomStatus(r.ID, status)
	if err != nil {
		return err
	}
	r.Status = status
	return nil
}

func (r *Room) SetSettings(settings model.RoomSettings) error {
	err := db.SaveRoomSettings(r.ID, settings)
	if err != nil {
		return err
	}
	r.Settings = settings
	return nil
}
