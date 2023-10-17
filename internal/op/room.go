package op

import (
	"errors"
	"net/url"
	"sync/atomic"
	"time"

	"github.com/go-resty/resty/v2"
	"github.com/google/uuid"
	"github.com/gorilla/websocket"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/av"
	"github.com/zijiren233/livelib/container/flv"
	rtmpProto "github.com/zijiren233/livelib/protocol/rtmp"
	"github.com/zijiren233/livelib/protocol/rtmp/core"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
	"golang.org/x/crypto/bcrypt"
)

type Room struct {
	model.Room
	version    uint32
	current    current
	rtmpa      *rtmps.App
	initOnce   utils.Once
	lastActive int64
	hub        *Hub
}

func (r *Room) Init() {
	r.initOnce.Do(func() {
		atomic.CompareAndSwapUint32(&r.version, 0, 1)
		r.current = *newCurrent()
		r.hub = newHub(r.ID)
		a, err := rtmp.RtmpServer().NewApp(r.Name)
		if err != nil {
			log.Fatalf("failed to create rtmp app: %s", err.Error())
		}
		r.rtmpa = a
	})
}

func (r *Room) Hub() *Hub {
	return r.hub
}

func (r *Room) App() *rtmps.App {
	return r.rtmpa
}

func (r *Room) close() {
	if r.initOnce.Done() {
		r.hub.Close()
		rtmp.RtmpServer().DelApp(r.Name)
	}
}

func (r *Room) Version() uint32 {
	return atomic.LoadUint32(&r.version)
}

func (r *Room) CheckVersion(version uint32) bool {
	return atomic.LoadUint32(&r.version) == version
}

func (r *Room) UpdateMovie(movieId uint, movie model.BaseMovieInfo) error {
	m, err := GetMovieByID(r.ID, movieId)
	if err != nil {
		return err
	}
	switch {
	case (m.RtmpSource && !movie.RtmpSource) || (m.Live && m.Proxy && !movie.Proxy):
		r.rtmpa.DelChannel(m.PullKey)
		m.PullKey = ""
	case m.Proxy && !movie.Proxy:
		m.PullKey = ""
	}
	return db.UpdateMovie(*m)
}

func (r *Room) InitMovie(movie *model.Movie) error {
	switch {
	case movie.RtmpSource && movie.Proxy:
		return errors.New("rtmp source and proxy can't be true at the same time")
	case movie.Live && movie.RtmpSource:
		if !conf.Conf.Rtmp.Enable {
			return errors.New("rtmp is not enabled")
		} else if movie.Type == "m3u8" && !conf.Conf.Rtmp.HlsPlayer {
			return errors.New("hls player is not enabled")
		}
		movie.PullKey = uuid.New().String()
		_, err := r.rtmpa.NewChannel(movie.PullKey)
		if err != nil {
			return err
		}
	case movie.Live && movie.Proxy:
		if !conf.Conf.Proxy.LiveProxy {
			return errors.New("live proxy is not enabled")
		}
		u, err := url.Parse(movie.Url)
		if err != nil {
			return err
		}
		switch u.Scheme {
		case "rtmp":
			PullKey := uuid.New().String()
			c, err := r.rtmpa.NewChannel(PullKey)
			if err != nil {
				return err
			}
			movie.PullKey = PullKey
			go func() {
				for {
					if c.Closed() {
						return
					}
					cli := core.NewConnClient()
					if err = cli.Start(movie.Url, av.PLAY); err != nil {
						cli.Close()
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(rtmpProto.NewReader(cli)); err != nil {
						cli.Close()
						time.Sleep(time.Second)
					}
				}
			}()
		case "http", "https":
			PullKey := uuid.New().String()
			c, err := r.rtmpa.NewChannel(PullKey)
			if err != nil {
				return err
			}
			movie.PullKey = PullKey
			go func() {
				for {
					if c.Closed() {
						return
					}
					r := resty.New().R()
					for k, v := range movie.Headers {
						r.SetHeader(k, v)
					}
					// r.SetHeader("User-Agent", UserAgent)
					resp, err := r.Get(movie.Url)
					if err != nil {
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(flv.NewReader(resp.RawBody())); err != nil {
						time.Sleep(time.Second)
					}
					resp.RawBody().Close()
				}
			}()
		default:
			return errors.New("unsupported scheme")
		}
	case !movie.Live && movie.RtmpSource:
		return errors.New("rtmp source can't be true when movie is not live")
	case !movie.Live && movie.Proxy:
		if !conf.Conf.Proxy.MovieProxy {
			return errors.New("movie proxy is not enabled")
		}
		movie.PullKey = uuid.New().String()
		fallthrough
	case !movie.Live && !movie.Proxy, movie.Live && !movie.Proxy && !movie.RtmpSource:
		u, err := url.Parse(movie.Url)
		if err != nil {
			return err
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			return errors.New("unsupported scheme")
		}
	default:
		return errors.New("unknown error")
	}
	return nil
}

func (r *Room) AddMovie(m model.MovieInfo) error {
	movie := &model.Movie{
		RoomID:    r.ID,
		MovieInfo: m,
	}

	err := r.InitMovie(movie)
	if err != nil {
		return err
	}

	return db.CreateMovie(movie)
}

func (r *Room) HasPermission(user *model.User, permission model.Permission) bool {
	ur, err := db.GetRoomUserRelation(r.ID, user.ID)
	if err != nil {
		return false
	}
	return ur.HasPermission(permission)
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
		atomic.AddUint32(&r.version, 1)
	}
	r.HashedPassword = hashedPassword
	return db.SetRoomHashedPassword(r.ID, hashedPassword)
}

func (r *Room) SetUserRole(userID uint, role model.Role) error {
	return db.SetUserRole(r.ID, userID, role)
}

func (r *Room) SetUserPermission(userID uint, permission model.Permission) error {
	return db.SetUserPermission(r.ID, userID, permission)
}

func (r *Room) AddUserPermission(userID uint, permission model.Permission) error {
	return db.AddUserPermission(r.ID, userID, permission)
}

func (r *Room) RemoveUserPermission(userID uint, permission model.Permission) error {
	return db.RemoveUserPermission(r.ID, userID, permission)
}

func (r *Room) GetMovies(conf ...db.GetMoviesConfig) ([]model.Movie, error) {
	return db.GetMoviesByRoomID(r.ID, conf...)
}

func (r *Room) GetMoviesCount() (int64, error) {
	return db.GetMoviesCountByRoomID(r.ID)
}

func (r *Room) GetAllMoviesByRoomID() ([]model.Movie, error) {
	return db.GetAllMoviesByRoomID(r.ID)
}

func (r *Room) GetMovieByID(id uint) (*model.Movie, error) {
	return db.GetMovieByID(r.ID, id)
}

func (r *Room) DeleteMovieByID(id uint) error {
	m, err := db.LoadAndDeleteMovieByID(r.ID, id)
	if err != nil {
		return err
	}
	if m.PullKey != "" {
		r.rtmpa.DelChannel(m.PullKey)
	}
	return nil
}

func (r *Room) ClearMovies() error {
	ms, err := db.LoadAndDeleteMoviesByRoomID(r.ID)
	if err != nil {
		return err
	}
	for _, m := range ms {
		if m.PullKey != "" {
			r.rtmpa.DelChannel(m.PullKey)
		}
	}
	return nil
}

func (r *Room) Current() *Current {
	c := r.current.Current()
	return &c
}

func (r *Room) ChangeCurrentMovie(id uint) error {
	m, err := db.GetMovieByID(r.ID, id)
	if err != nil {
		return err
	}
	r.current.SetMovie(*m)
	return nil
}

func (r *Room) SwapMoviePositions(id1, id2 uint) error {
	return db.SwapMoviePositions(r.ID, id1, id2)
}

func (r *Room) GetMovieWithPullKey(pullKey string) (*model.Movie, error) {
	return db.GetMovieWithPullKey(r.ID, pullKey)
}

func (r *Room) RegClient(user *User, conn *websocket.Conn) (*Client, error) {
	return r.hub.RegClient(newClient(user, r, conn))
}

func (r *Room) UnregisterClient(user *User) error {
	return r.hub.UnRegClient(user)
}

func (r *Room) SetStatus(playing bool, seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetStatus(playing, seek, rate, timeDiff)
}

func (r *Room) SetSeekRate(seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetSeekRate(seek, rate, timeDiff)
}
