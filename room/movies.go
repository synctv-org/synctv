package room

import (
	"fmt"
	"net/url"
	"sync"
	"time"

	"github.com/zijiren233/gencontainer/dllist"
	rtmps "github.com/zijiren233/livelib/server"
)

type FormatErrMovieNotFound uint64

func (e FormatErrMovieNotFound) Error() string {
	return fmt.Sprintf("movie id: %v not found", uint64(e))
}

type FormatErrMovieAlreadyExist uint64

func (e FormatErrMovieAlreadyExist) Error() string {
	return fmt.Sprintf("movie id: %v already exist", uint64(e))
}

type movies struct {
	l    *dllist.Dllist[*Movie]
	lock sync.RWMutex
}

// Url will be `PullKey` when Live and Proxy are true
type Movie struct {
	BaseMovie
	PullKey    string `json:"pullKey"`
	CreateAt   int64  `json:"createAt"`
	LastEditAt int64  `json:"lastEditAt"`

	id      uint64
	channel *rtmps.Channel
	creator *User
}

type BaseMovie struct {
	Url        string            `json:"url"`
	Name       string            `json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `json:"type"`
	Headers    map[string]string `json:"headers"`
}

type MovieConf func(m *Movie)

func WithPullKey(PullKey string) MovieConf {
	return func(m *Movie) {
		m.PullKey = PullKey
	}
}

func WithChannel(channel *rtmps.Channel) MovieConf {
	return func(m *Movie) {
		m.channel = channel
	}
}

func WithCreator(creator *User) MovieConf {
	return func(m *Movie) {
		m.creator = creator
	}
}

func NewMovie(id uint64, url, name, type_ string, live, proxy, rtmpSource bool, headers map[string]string, conf ...MovieConf) (*Movie, error) {
	return NewMovieWithBaseMovie(id, BaseMovie{
		Url:        url,
		Name:       name,
		Live:       live,
		Proxy:      proxy,
		RtmpSource: rtmpSource,
		Type:       type_,
		Headers:    headers,
	})
}

func NewMovieWithBaseMovie(id uint64, baseMovie BaseMovie, conf ...MovieConf) (*Movie, error) {
	now := time.Now().UnixMicro()
	m := &Movie{
		id:         id,
		BaseMovie:  baseMovie,
		CreateAt:   now,
		LastEditAt: now,
	}
	m.Init(conf...)
	return m, m.Check()
}

func (m *Movie) Check() error {
	_, err := url.Parse(m.Url)
	if err != nil {
		return err
	}
	return nil
}

func (m *Movie) Id() uint64 {
	return m.id
}

func (m *Movie) Init(conf ...MovieConf) {
	for _, c := range conf {
		c(m)
	}
}

func (m *Movie) Creator() *User {
	return m.creator
}

func (m *Movie) SetCreator(creator *User) {
	m.creator = creator
}

func (m *Movie) Channel() *rtmps.Channel {
	return m.channel
}

func (m *Movie) SetChannel(channel *rtmps.Channel) {
	m.channel = channel
}

func newMovies() *movies {
	return &movies{l: dllist.New[*Movie]()}
}

func (m *movies) Range(f func(e *dllist.Element[*Movie]) bool) (interrupt bool) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	interrupt = false
	for e := m.l.Front(); !interrupt && e != nil; e = e.Next() {
		interrupt = !f(e)
	}
	return
}

func (m *movies) range_(f func(e *dllist.Element[*Movie]) bool) (interrupt bool) {
	interrupt = false
	for e := m.l.Front(); !interrupt && e != nil; e = e.Next() {
		interrupt = !f(e)
	}
	return
}

type MovieInfo struct {
	Id         uint64            `json:"id"`
	Url        string            `json:"url"`
	Name       string            `json:"name"`
	Live       bool              `json:"live"`
	Proxy      bool              `json:"proxy"`
	RtmpSource bool              `json:"rtmpSource"`
	Type       string            `json:"type"`
	Headers    map[string]string `json:"headers"`
	PullKey    string            `json:"pullKey"`
	CreateAt   int64             `json:"createAt"`
	LastEditAt int64             `json:"lastEditAt"`
	Creator    string            `json:"creator"`
}

func (m *movies) MovieList() (movies []MovieInfo) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	movies = make([]MovieInfo, 0, m.l.Len())
	m.range_(func(e *dllist.Element[*Movie]) bool {
		movies = append(movies, MovieInfo{
			Id:         e.Value.id,
			Url:        e.Value.Url,
			Name:       e.Value.Name,
			Live:       e.Value.Live,
			Proxy:      e.Value.Proxy,
			RtmpSource: e.Value.RtmpSource,
			Type:       e.Value.Type,
			Headers:    e.Value.Headers,
			CreateAt:   e.Value.CreateAt,
			LastEditAt: e.Value.LastEditAt,
			Creator:    e.Value.Creator().Name(),
		})
		return true
	})
	return
}

func (m *movies) Clear() error {
	m.lock.Lock()
	defer m.lock.Unlock()

	m.l.Clear()
	return nil
}

func (m *movies) GetAndClear() (movies []*Movie) {
	m.lock.Lock()
	defer m.lock.Unlock()

	movies = make([]*Movie, 0, m.l.Len())
	m.range_(func(e *dllist.Element[*Movie]) bool {
		movies = append(movies, e.Value)
		return true
	})
	m.l.Clear()
	return
}

func (m *movies) HasMovie(id uint64) bool {
	m.lock.RLock()
	defer m.lock.RUnlock()

	return m.hasMovie(id)
}

func (m *movies) hasMovie(id uint64) bool {
	return m.range_(func(e *dllist.Element[*Movie]) bool {
		return e.Value.id != id
	})
}

func (m *movies) GetMovie(id uint64) (movie *Movie, err error) {
	m.lock.RLock()
	defer m.lock.RUnlock()

	e, err := m.getMovie(id)
	if err != nil {
		return nil, FormatErrMovieNotFound(id)
	}
	return e.Value, nil
}

func (m *movies) GetMovieWithPullKey(pullKey string) (movie *Movie, err error) {
	if pullKey == "" {
		return nil, fmt.Errorf("pullKey is empty")
	}

	m.lock.RLock()
	defer m.lock.RUnlock()

	m.range_(func(e *dllist.Element[*Movie]) bool {
		if e.Value.PullKey == pullKey {
			movie = e.Value
			return false
		}
		return true
	})
	if movie == nil {
		return nil, fmt.Errorf("pullKey: %v not found", pullKey)
	}
	return
}

func (m *movies) GetAndDelMovie(id ...uint64) (movies []*Movie, err error) {
	m.lock.Lock()
	defer m.lock.Unlock()

	es := make([]*dllist.Element[*Movie], 0, len(id))

	for _, v := range id {
		e, err := m.getMovie(v)
		if err != nil {
			return nil, FormatErrMovieNotFound(v)
		}
		es = append(es, e)
	}

	movies = make([]*Movie, 0, len(es))
	for _, e := range es {
		movies = append(movies, e.Value)
		e.Remove()
	}
	return movies, nil
}

func (m *movies) getMovie(id uint64) (element *dllist.Element[*Movie], err error) {
	err = FormatErrMovieNotFound(id)
	m.range_(func(e *dllist.Element[*Movie]) bool {
		if e.Value.id == id {
			element = e
			err = nil
			return false
		}
		return true
	})
	return
}

func (m *movies) PushBackMovie(movie *Movie) error {
	m.lock.Lock()
	defer m.lock.Unlock()

	if m.hasMovie(movie.id) {
		return FormatErrMovieAlreadyExist(movie.id)
	}

	m.l.PushBack(movie)

	return nil
}

func (m *movies) PushFrontMovie(movie *Movie) error {
	m.lock.Lock()
	defer m.lock.Unlock()

	if m.hasMovie(movie.id) {
		return FormatErrMovieAlreadyExist(movie.id)
	}

	m.l.PushFront(movie)

	return nil
}

func (m *movies) DelMovie(id uint64) error {
	m.lock.Lock()
	defer m.lock.Unlock()

	e, err := m.getMovie(id)
	if err != nil {
		return err
	}
	e.Remove()

	return nil
}

func (m *movies) SwapMovie(id1, id2 uint64) error {
	m.lock.Lock()
	defer m.lock.Unlock()

	e1, err := m.getMovie(id1)
	if err != nil {
		return err
	}

	e2, err := m.getMovie(id2)
	if err != nil {
		return err
	}
	m.l.Swap(e1, e2)
	return nil
}
