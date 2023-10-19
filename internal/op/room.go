package op

import (
	"errors"
	"net/url"
	"strconv"
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
	current    *current
	rtmpa      *rtmps.App
	initOnce   utils.Once
	lastActive int64
	hub        *Hub
}

func (r *Room) LazyInit() (err error) {
	r.initOnce.Do(func() {
		r.hub = newHub(r.ID)
		r.rtmpa, err = rtmp.RtmpServer().NewApp(strconv.Itoa(int(r.ID)))
		if err != nil {
			log.Errorf("failed to create rtmp app: %s", err.Error())
			return
		}

		var ms []*model.Movie
		ms, err = r.GetAllMoviesByRoomID()
		if err != nil {
			log.Errorf("failed to get movies: %s", err.Error())
			return
		}
		for _, m := range ms {
			if err = r.initMovie(m); err != nil {
				log.Errorf("failed to init movie: %s", err.Error())
				return
			}
		}
	})
	return
}

func (r *Room) ClientNum() int64 {
	if r.hub == nil {
		return 0
	}
	return r.hub.ClientNum()
}

func (r *Room) Broadcast(data Message, conf ...BroadcastConf) error {
	if r.hub == nil {
		return nil
	}
	return r.hub.Broadcast(data, conf...)
}

func (r *Room) GetChannel(channelName string) (*rtmps.Channel, error) {
	err := r.LazyInit()
	if err != nil {
		return nil, err
	}

	return r.rtmpa.GetChannel(channelName)
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
	err := r.LazyInit()
	if err != nil {
		return err
	}

	m, err := GetMovieByID(r.ID, movieId)
	if err != nil {
		return err
	}

	err = r.terminateMovie(m)
	if err != nil {
		return err
	}

	m.MovieInfo.BaseMovieInfo = movie

	err = r.initMovie(m)
	if err != nil {
		return err
	}

	return SaveMovie(m)
}

func (r *Room) terminateMovie(movie *model.Movie) error {
	switch {
	case movie.Live && movie.RtmpSource:
		if movie.PullKey != "" {
			r.rtmpa.DelChannel(movie.PullKey)
		}
	case movie.Live && movie.Proxy:
		if movie.PullKey != "" {
			r.rtmpa.DelChannel(movie.PullKey)
		}
	}
	return nil
}

func (r *Room) initMovie(movie *model.Movie) error {
	switch {
	case movie.RtmpSource && movie.Proxy:
		return errors.New("rtmp source and proxy can't be true at the same time")
	case movie.Live && movie.RtmpSource:
		if !conf.Conf.Rtmp.Enable {
			return errors.New("rtmp is not enabled")
		}
		if movie.PullKey == "" {
			movie.PullKey = uuid.New().String()
		}
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
			movie.PullKey = uuid.NewMD5(uuid.NameSpaceURL, []byte(movie.Url)).String()
			c, err := r.rtmpa.NewChannel(movie.PullKey)
			if err != nil {
				return err
			}
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
			movie.PullKey = uuid.NewMD5(uuid.NameSpaceURL, []byte(movie.Url)).String()
			c, err := r.rtmpa.NewChannel(movie.PullKey)
			if err != nil {
				return err
			}
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
		u, err := url.Parse(movie.Url)
		if err != nil {
			return err
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			return errors.New("unsupported scheme")
		}
		movie.PullKey = uuid.NewMD5(uuid.NameSpaceURL, []byte(movie.Url)).String()
	case !movie.Live && !movie.Proxy, movie.Live && !movie.Proxy && !movie.RtmpSource:
		u, err := url.Parse(movie.Url)
		if err != nil {
			return err
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			return errors.New("unsupported scheme")
		}
		movie.PullKey = ""
	default:
		return errors.New("unknown error")
	}
	return nil
}

func (r *Room) AddMovie(m model.MovieInfo) error {
	err := r.LazyInit()
	if err != nil {
		return err
	}

	movie := &model.Movie{
		RoomID:    r.ID,
		Position:  uint(time.Now().UnixMilli()),
		MovieInfo: m,
	}

	err = r.initMovie(movie)
	if err != nil {
		return err
	}

	return CreateMovie(movie)
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

func (r *Room) GetMoviesCount() (int, error) {
	return GetMoviesCountByRoomID(r.ID)
}

func (r *Room) GetAllMoviesByRoomID() ([]*model.Movie, error) {
	ms, err := GetAllMoviesByRoomID(r.ID)
	if err != nil {
		return nil, err
	}
	var m []*model.Movie = make([]*model.Movie, 0, ms.Len())
	for i := ms.Front(); i != nil; i = i.Next() {
		m = append(m, i.Value)
	}
	return m, nil
}

func (r *Room) GetMoviesByRoomIDWithPage(page, pageSize int) ([]*model.Movie, error) {
	return GetMoviesByRoomIDWithPage(r.ID, page, pageSize)
}

func (r *Room) GetMovieByID(id uint) (*model.Movie, error) {
	return GetMovieByID(r.ID, id)
}

func (r *Room) DeleteMovieByID(id uint) error {
	r.LazyInit()
	m, err := LoadAndDeleteMovieByID(r.ID, id)
	if err != nil {
		return err
	}
	return r.terminateMovie(m)
}

func (r *Room) ClearMovies() error {
	r.LazyInit()
	ms, err := db.LoadAndDeleteMoviesByRoomID(r.ID)
	if err != nil {
		return err
	}
	for _, m := range ms {
		r.terminateMovie(m)
	}
	return nil
}

func (r *Room) Current() *Current {
	c := r.current.Current()
	return &c
}

func (r *Room) ChangeCurrentMovie(id uint) error {
	r.LazyInit()
	m, err := GetMovieByID(r.ID, id)
	if err != nil {
		return err
	}
	r.current.SetMovie(*m)
	return nil
}

func (r *Room) SwapMoviePositions(id1, id2 uint) error {
	r.LazyInit()
	return SwapMoviePositions(r.ID, id1, id2)
}

func (r *Room) GetMovieWithPullKey(pullKey string) (*model.Movie, error) {
	return GetMovieWithPullKey(r.ID, pullKey)
}

func (r *Room) RegClient(user *User, conn *websocket.Conn) (*Client, error) {
	r.LazyInit()
	return r.hub.RegClient(newClient(user, r, conn))
}

func (r *Room) UnregisterClient(user *User) error {
	r.LazyInit()
	return r.hub.UnRegClient(user)
}

func (r *Room) SetStatus(playing bool, seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetStatus(playing, seek, rate, timeDiff)
}

func (r *Room) SetSeekRate(seek float64, rate float64, timeDiff float64) Status {
	return r.current.SetSeekRate(seek, rate, timeDiff)
}
