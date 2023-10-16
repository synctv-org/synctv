package room

import (
	"errors"
	"math/rand"
	"sync"
	"sync/atomic"
	"time"

	"github.com/gorilla/websocket"
	"github.com/zijiren233/gencontainer/rwmap"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

var (
	ErrRoomIdEmpty        = errors.New("roomid is empty")
	ErrRoomIdTooLong      = errors.New("roomid is too long")
	ErrAdminPassWordEmpty = errors.New("admin password is empty")
)

type Room struct {
	id         string
	settings   settings
	password   []byte
	version    uint64
	current    *current
	rtmps      *rtmps.Server
	rtmpa      *rtmps.App
	initOnce   sync.Once
	users      rwmap.RWMap[string, *User]
	rootUser   *User
	lastActive int64
	createdAt  int64
	mid        uint64
	hub        *hub
	*movies
}

type RoomConf func(r *Room)

func WithVersion(version uint64) RoomConf {
	return func(r *Room) {
		r.version = version
	}
}

func WithHidden(hidden bool) RoomConf {
	return func(r *Room) {
		r.SetHidden(hidden)
	}
}

func WithRootUser(u *User) RoomConf {
	return func(r *Room) {
		u.admin = true
		u.room = r
		r.rootUser = u
		r.AddUser(u)
	}
}

// Version cant is 0
func NewRoom(RoomId string, Password string, rtmps *rtmps.Server, conf ...RoomConf) (*Room, error) {
	if RoomId == "" {
		return nil, ErrRoomIdEmpty
	} else if len(RoomId) > 32 {
		return nil, ErrRoomIdTooLong
	}
	now := time.Now().UnixMilli()
	r := &Room{
		id:         RoomId,
		rtmps:      rtmps,
		lastActive: now,
		createdAt:  now,
	}

	for _, c := range conf {
		c(r)
	}

	if r.version == 0 {
		r.version = rand.New(rand.NewSource(now)).Uint64()
	}

	return r, r.SetPassword(Password)
}

func (r *Room) Init() {
	r.initOnce.Do(func() {
		r.rtmpa = r.rtmps.GetOrNewApp(r.id)
		r.hub = newHub(r.id)
		r.movies = newMovies()
		r.current = newCurrent()
	})
}

func (r *Room) CreatedAt() int64 {
	return atomic.LoadInt64(&r.createdAt)
}

func (r *Room) RootUser() *User {
	return r.rootUser
}

func (r *Room) SetRootUser(u *User) {
	r.rootUser = u
}

func (r *Room) NewUser(id string, password string, conf ...UserConf) (*User, error) {
	u, err := NewUser(id, password, r, conf...)
	if err != nil {
		return nil, err
	}
	_, loaded := r.users.LoadOrStore(u.name, u)
	if loaded {
		return nil, errors.New("user already exist")
	}
	return u, nil
}

func (r *Room) AddUser(u *User) error {
	_, loaded := r.users.LoadOrStore(u.name, u)
	if loaded {
		return errors.New("user already exist")
	}
	return nil
}

func (r *Room) GetUser(id string) (*User, error) {
	u, ok := r.users.Load(id)
	if !ok {
		return nil, errors.New("user not found")
	}
	return u, nil
}

func (r *Room) DelUser(id string) error {
	_, ok := r.users.LoadAndDelete(id)
	if !ok {
		return errors.New("user not found")
	}
	return nil
}

func (r *Room) GetAndDelUser(id string) (u *User, ok bool) {
	return r.users.LoadAndDelete(id)
}

func (r *Room) GetOrNewUser(id string, password string, conf ...UserConf) (*User, error) {
	u, err := NewUser(id, password, r, conf...)
	if err != nil {
		return nil, err
	}
	user, _ := r.users.LoadOrStore(u.name, u)
	return user, nil
}

func (r *Room) UserList() (users []User) {
	users = make([]User, 0, r.users.Len())
	r.users.Range(func(name string, u *User) bool {
		users = append(users, *u)
		return true
	})
	return
}

func (r *Room) NewLiveChannel(channel string) (*rtmps.Channel, error) {
	c, err := r.rtmpa.NewChannel(channel)
	if err != nil {
		return nil, err
	}
	return c, nil
}

func (r *Room) Start() {
	go r.Serve()
}

func (r *Room) Serve() {
	r.hub.Serve()
}

func (r *Room) Close() error {
	if err := r.hub.Close(); err != nil {
		return err
	}
	err := r.rtmps.DelApp(r.id)
	if err != nil {
		return err
	}
	return nil
}

func (r *Room) SetHidden(hidden bool) {
	r.settings.SetHidden(hidden)
}

func (r *Room) Hidden() bool {
	return r.settings.Hidden()
}

func (r *Room) Id() string {
	return r.id
}

func (r *Room) UpdateActiveTime() {
	atomic.StoreInt64(&r.lastActive, time.Now().UnixMilli())
}

func (r *Room) LateActiveTime() int64 {
	return atomic.LoadInt64(&r.lastActive)
}

func (r *Room) SetPassword(password string) error {
	if password != "" {
		b, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
		r.password = b
		r.settings.SetNeedPassword(true)
	} else {
		r.settings.SetNeedPassword(false)
		r.password = nil
	}
	r.updateVersion()
	return nil
}

func (r *Room) SetPasswordAndCloseAll(password string) error {
	err := r.SetPassword(password)
	if err != nil {
		return err
	}
	r.hub.clients.Range(func(_ string, value *Client) bool {
		value.Close()
		return true
	})
	return nil
}

func (r *Room) CheckPassword(password string) (ok bool) {
	if !r.NeedPassword() {
		return true
	}
	return bcrypt.CompareHashAndPassword(r.password, stream.StringToBytes(password)) == nil
}

func (r *Room) NeedPassword() bool {
	return r.settings.NeedPassword()
}

func (r *Room) Version() uint64 {
	return atomic.LoadUint64(&r.version)
}

func (r *Room) CheckVersion(version uint64) bool {
	return r.Version() == version
}

func (r *Room) SetVersion(version uint64) {
	atomic.StoreUint64(&r.version, version)
}

func (r *Room) updateVersion() uint64 {
	return atomic.AddUint64(&r.version, 1)
}

func (r *Room) Current() *Current {
	c := r.current.Current()
	return &c
}

// Seek will be set to 0
func (r *Room) ChangeCurrentMovie(id uint64) error {
	r.UpdateActiveTime()
	e, err := r.movies.getMovie(id)
	if err != nil {
		return err
	}
	r.current.SetMovie(MovieInfo{
		Id: e.Value.id,
		BaseMovieInfo: BaseMovieInfo{
			Url:        e.Value.Url,
			Name:       e.Value.Name,
			Live:       e.Value.Live,
			Proxy:      e.Value.Proxy,
			RtmpSource: e.Value.RtmpSource,
			Type:       e.Value.Type,
			Headers:    e.Value.Headers,
		},
		PullKey:   e.Value.PullKey,
		CreatedAt: e.Value.CreatedAt,
		Creator:   e.Value.Creator().Name(),
	})
	return nil
}

func (r *Room) SetStatus(playing bool, seek, rate, timeDiff float64) Status {
	r.UpdateActiveTime()
	return r.current.SetStatus(playing, seek, rate, timeDiff)
}

func (r *Room) SetSeekRate(seek, rate, timeDiff float64) Status {
	r.UpdateActiveTime()
	return r.current.SetSeekRate(seek, rate, timeDiff)
}

func (r *Room) PushBackMovie(movie *Movie) error {
	if r.hub.Closed() {
		return ErrAlreadyClosed
	}
	r.UpdateActiveTime()

	return r.movies.PushBackMovie(movie)
}

func (r *Room) PushFrontMovie(movie *Movie) error {
	r.UpdateActiveTime()

	return r.movies.PushFrontMovie(movie)
}

func (r *Room) DelMovie(id ...uint64) error {
	r.UpdateActiveTime()
	m, err := r.movies.GetAndDelMovie(id...)
	if err != nil {
		return err
	}
	return r.closeLive(m)
}

func (r *Room) ClearMovies() (err error) {
	r.UpdateActiveTime()
	return r.closeLive(r.movies.GetAndClear())
}

func (r *Room) closeLive(m []*Movie) error {
	for _, m := range m {
		if m.RtmpSource || (m.Proxy && m.Live) {
			if err := r.rtmpa.DelChannel(m.PullKey); err != nil {
				return err
			}
		}
	}
	return nil
}

func (r *Room) SwapMovie(id1, id2 uint64) error {
	r.UpdateActiveTime()
	return r.movies.SwapMovie(id1, id2)
}

func (r *Room) Broadcast(msg Message, conf ...BroadcastConf) error {
	r.UpdateActiveTime()
	return r.hub.Broadcast(msg, conf...)
}

func (r *Room) RegClient(user *User, conn *websocket.Conn) (*Client, error) {
	r.UpdateActiveTime()
	return r.hub.RegClient(user, conn)
}

func (r *Room) UnRegClient(user *User) error {
	r.UpdateActiveTime()
	return r.hub.UnRegClient(user)
}

func (r *Room) Closed() bool {
	return r.hub.Closed()
}

func (r *Room) ClientNum() int64 {
	r.UpdateActiveTime()
	return r.hub.ClientNum()
}
