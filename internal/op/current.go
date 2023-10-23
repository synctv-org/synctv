package op

import (
	"sync"
	"time"

	"github.com/synctv-org/synctv/internal/model"
	pb "github.com/synctv-org/synctv/proto/message"
)

type current struct {
	current Current
	lock    sync.RWMutex
}

type Current struct {
	Movie  model.Movie `json:"movie"`
	Status Status      `json:"status"`
}

func newCurrent() *current {
	return &current{
		current: Current{
			Status: newStatus(),
		},
	}
}

type Status struct {
	Seek       float64 `json:"seek"`
	Rate       float64 `json:"rate"`
	Playing    bool    `json:"playing"`
	lastUpdate time.Time
}

func newStatus() Status {
	return Status{
		Seek:       0,
		Rate:       1.0,
		lastUpdate: time.Now(),
	}
}

func (c *current) Current() Current {
	c.lock.RLock()
	defer c.lock.RUnlock()
	c.current.updateSeek()
	return c.current
}

func (c *current) Movie() model.Movie {
	c.lock.RLock()
	defer c.lock.RUnlock()

	return c.current.Movie
}

func (c *current) SetMovie(movie model.Movie) {
	c.lock.Lock()
	defer c.lock.Unlock()

	c.current.Movie = movie
	c.current.SetSeek(0, 0)
	c.current.Status.Playing = true
}

func (c *current) Status() Status {
	c.lock.RLock()
	defer c.lock.RUnlock()
	c.current.updateSeek()
	return c.current.Status
}

func (c *current) SetStatus(playing bool, seek, rate, timeDiff float64) Status {
	c.lock.Lock()
	defer c.lock.Unlock()

	return c.current.SetStatus(playing, seek, rate, timeDiff)
}

func (c *current) SetSeekRate(seek, rate, timeDiff float64) Status {
	c.lock.Lock()
	defer c.lock.Unlock()

	return c.current.SetSeekRate(seek, rate, timeDiff)
}

func (c *Current) Proto() *pb.Current {
	return &pb.Current{
		Movie: &pb.MovieInfo{
			Id: uint64(c.Movie.ID),
			Base: &pb.BaseMovieInfo{
				Url:        c.Movie.Base.Url,
				Name:       c.Movie.Base.Name,
				Live:       c.Movie.Base.Live,
				Proxy:      c.Movie.Base.Proxy,
				RtmpSource: c.Movie.Base.RtmpSource,
				Type:       c.Movie.Base.Type,
				Headers:    c.Movie.Base.Headers,
			},
			PullKey:   c.Movie.PullKey,
			CreatedAt: c.Movie.CreatedAt.UnixMilli(),
			Creator:   GetUserName(c.Movie.CreatorID),
		},
		Status: &pb.Status{
			Seek:    c.Status.Seek,
			Rate:    c.Status.Rate,
			Playing: c.Status.Playing,
		},
	}
}

func (c *Current) updateSeek() {
	if c.Movie.Base.Live {
		c.Status.lastUpdate = time.Now()
		return
	}
	if c.Status.Playing {
		c.Status.Seek += time.Since(c.Status.lastUpdate).Seconds() * c.Status.Rate
	}
	c.Status.lastUpdate = time.Now()
}

func (c *Current) setLiveStatus() Status {
	c.Status.Playing = true
	c.Status.Rate = 1.0
	c.Status.Seek = 0
	c.Status.lastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetStatus(playing bool, seek, rate, timeDiff float64) Status {
	if c.Movie.Base.Live {
		return c.setLiveStatus()
	}
	c.Status.Playing = playing
	c.Status.Rate = rate
	if playing {
		c.Status.Seek = seek + (timeDiff * rate)
	} else {
		c.Status.Seek = seek
	}
	c.Status.lastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetSeekRate(seek, rate, timeDiff float64) Status {
	if c.Movie.Base.Live {
		return c.setLiveStatus()
	}
	if c.Status.Playing {
		c.Status.Seek = seek + (timeDiff * rate)
	} else {
		c.Status.Seek = seek
	}
	c.Status.Rate = rate
	c.Status.lastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetSeek(seek, timeDiff float64) Status {
	if c.Movie.Base.Live {
		return c.setLiveStatus()
	}
	if c.Status.Playing {
		c.Status.Seek = seek + (timeDiff * c.Status.Rate)
	} else {
		c.Status.Seek = seek
	}
	c.Status.lastUpdate = time.Now()
	return c.Status
}
