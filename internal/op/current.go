package op

import (
	"sync"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
)

type current struct {
	roomID  string
	current model.Current
	lock    sync.RWMutex
}

func newCurrent(roomID string, c *model.Current) *current {
	if c == nil {
		return &current{
			roomID: roomID,
			current: model.Current{
				Status: model.NewStatus(),
			},
		}
	}
	return &current{
		roomID:  roomID,
		current: *c,
	}
}

func (c *current) Current() model.Current {
	c.lock.RLock()
	defer c.lock.RUnlock()
	c.current.UpdateStatus()
	return c.current
}

func (c *current) CurrentMovie() model.CurrentMovie {
	c.lock.RLock()
	defer c.lock.RUnlock()
	return c.current.Movie
}

func (c *current) SetMovie(movie model.CurrentMovie, play bool) {
	c.lock.Lock()
	defer c.lock.Unlock()
	defer db.SetRoomCurrent(c.roomID, &c.current)

	c.current.Movie = movie
	c.current.SetSeek(0, 0)
	c.current.Status.IsPlaying = play
}

func (c *current) Status() model.Status {
	c.lock.RLock()
	defer c.lock.RUnlock()
	c.current.UpdateStatus()
	return c.current.Status
}

func (c *current) SetStatus(playing bool, seek, rate, timeDiff float64) *model.Status {
	c.lock.Lock()
	defer c.lock.Unlock()
	defer db.SetRoomCurrent(c.roomID, &c.current)

	s := c.current.SetStatus(playing, seek, rate, timeDiff)
	return &s
}

func (c *current) SetSeekRate(seek, rate, timeDiff float64) *model.Status {
	c.lock.Lock()
	defer c.lock.Unlock()
	defer db.SetRoomCurrent(c.roomID, &c.current)

	s := c.current.SetSeekRate(seek, rate, timeDiff)
	return &s
}
