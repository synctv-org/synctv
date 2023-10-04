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
	ErrRoomIDEmpty        = errors.New("roomid is empty")
	ErrAdminPassWordEmpty = errors.New("admin password is empty")
)

type Room struct {
	id                string
	lock              *sync.RWMutex
	password          []byte
	needPassword      bool
	version           uint64
	current           *Current
	maxInactivityTime time.Duration
	lastActive        time.Time
	rtmps             *rtmps.Server
	rtmpa             *rtmps.App
	hidden            bool
	timer             *time.Timer
	inited            bool
	users             *rwmap.RWMap[string, *User]
	rootUser          *User
	createAt          time.Time
	mid               uint64
	*movies
	*hub
}

type RoomConf func(r *Room)

func WithVersion(version uint64) RoomConf {
	return func(r *Room) {
		r.version = version
	}
}

func WithMaxInactivityTime(maxInactivityTime time.Duration) RoomConf {
	return func(r *Room) {
		r.maxInactivityTime = maxInactivityTime
	}
}

func WithHidden(hidden bool) RoomConf {
	return func(r *Room) {
		r.hidden = hidden
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
func NewRoom(RoomID string, Password string, rtmps *rtmps.Server, conf ...RoomConf) (*Room, error) {
	if RoomID == "" {
		return nil, ErrRoomIDEmpty
	}

	r := &Room{
		id:                RoomID,
		lock:              new(sync.RWMutex),
		movies:            newMovies(),
		current:           newCurrent(),
		maxInactivityTime: 12 * time.Hour,
		lastActive:        time.Now(),
		hub:               newHub(RoomID),
		rtmps:             rtmps,
		users:             &rwmap.RWMap[string, *User]{},
		createAt:          time.Now(),
	}

	for _, c := range conf {
		c(r)
	}

	if r.version == 0 {
		r.version = rand.New(rand.NewSource(time.Now().UnixNano())).Uint64()
	}

	return r, r.SetPassword(Password)
}

func (r *Room) CreateAt() time.Time {
	return r.createAt
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
	r.init()
	r.hub.Serve()
}

func (r *Room) init() {
	r.lock.Lock()
	defer r.lock.Unlock()
	if r.inited {
		return
	}
	r.inited = true
	if r.maxInactivityTime != 0 {
		r.timer = time.AfterFunc(time.Duration(r.maxInactivityTime), func() {
			r.Close()
		})
	}
	r.rtmpa = r.rtmps.GetOrNewApp(r.id)
}

func (r *Room) Close() error {
	r.lock.Lock()
	defer r.lock.Unlock()
	if err := r.hub.Close(); err != nil {
		return err
	}
	err := r.rtmps.DelApp(r.id)
	if err != nil {
		return err
	}
	if r.timer != nil {
		r.timer.Stop()
	}
	return nil
}

func (r *Room) SetHidden(hidden bool) {
	r.lock.Lock()
	defer r.lock.Unlock()
	r.hidden = hidden
}

func (r *Room) Hidden() bool {
	r.lock.RLock()
	defer r.lock.RUnlock()
	return r.hidden
}

func (r *Room) ID() string {
	return r.id
}

func (r *Room) UpdateActiveTime() {
	r.lock.Lock()
	defer r.lock.Unlock()
	r.updateActiveTime()
}

func (r *Room) updateActiveTime() {
	if r.maxInactivityTime != 0 {
		r.timer.Reset(r.maxInactivityTime)
	}
	r.lastActive = time.Now()
}

func (r *Room) ResetMaxInactivityTime(maxInactivityTime time.Duration) {
	r.lock.Lock()
	defer r.lock.Unlock()
	r.maxInactivityTime = maxInactivityTime
	r.updateActiveTime()
}

func (r *Room) LateActiveTime() time.Time {
	r.lock.RLock()
	defer r.lock.RUnlock()
	return r.lastActive
}

func (r *Room) SetPassword(password string) error {
	r.lock.Lock()
	defer r.lock.Unlock()
	if password != "" {
		b, err := bcrypt.GenerateFromPassword(stream.StringToBytes(password), bcrypt.DefaultCost)
		if err != nil {
			return err
		}
		r.password = b
		r.needPassword = true
	} else {
		r.needPassword = false
	}
	r.updateVersion()
	r.hub.clients.Range(func(_ string, value *Client) bool {
		value.Close()
		return true
	})
	return nil
}

func (r *Room) CheckPassword(password string) (ok bool) {
	r.lock.RLock()
	defer r.lock.RUnlock()
	if !r.needPassword {
		return true
	}
	return bcrypt.CompareHashAndPassword(r.password, stream.StringToBytes(password)) == nil
}

func (r *Room) NeedPassword() bool {
	r.lock.RLock()
	defer r.lock.RUnlock()
	return r.needPassword
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
	return r.current
}

// Seek will be set to 0
func (r *Room) ChangeCurrentMovie(id uint64) error {
	e, err := r.movies.getMovie(id)
	if err != nil {
		return err
	}
	r.current.SetMovie(MovieInfo{
		Id:         e.Value.id,
		Url:        e.Value.Url,
		Name:       e.Value.Name,
		Live:       e.Value.Live,
		Proxy:      e.Value.Proxy,
		RtmpSource: e.Value.RtmpSource,
		Type:       e.Value.Type,
		Headers:    e.Value.Headers,
		PullKey:    e.Value.PullKey,
		CreateAt:   e.Value.CreateAt,
		LastEditAt: e.Value.LastEditAt,
		Creator:    e.Value.Creator().Name(),
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

	return r.movies.PushBackMovie(movie)
}

func (r *Room) PushFrontMovie(movie *Movie) error {
	if r.hub.Closed() {
		return ErrAlreadyClosed
	}

	return r.movies.PushFrontMovie(movie)
}

func (r *Room) DelMovie(id ...uint64) error {
	if r.hub.Closed() {
		return ErrAlreadyClosed
	}
	m, err := r.movies.GetAndDelMovie(id...)
	if err != nil {
		return err
	}
	return r.closeLive(m)
}

func (r *Room) ClearMovies() (err error) {
	if r.hub.Closed() {
		return ErrAlreadyClosed
	}
	return r.closeLive(r.movies.GetAndClear())
}

func (r *Room) closeLive(m []*Movie) error {
	for _, m := range m {
		if m.Live {
			if err := r.rtmpa.DelChannel(m.PullKey); err != nil {
				return err
			}
		}
	}
	return nil
}

func (r *Room) SwapMovie(id1, id2 uint64) error {
	if r.hub.Closed() {
		return ErrAlreadyClosed
	}
	return r.movies.SwapMovie(id1, id2)
}

func (r *Room) Broadcast(msg Message, conf ...BroadcastConf) error {
	r.UpdateActiveTime()
	return r.hub.Broadcast(msg, conf...)
}

func (r *Room) RegClient(user *User, conn *websocket.Conn) (*Client, error) {
	r.updateActiveTime()
	return r.hub.RegClient(user, conn)
}
