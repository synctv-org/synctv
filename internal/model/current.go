package model

import "time"

type Current struct {
	Movie  CurrentMovie `json:"movie"`
	Status Status       `json:"status"`
}

type CurrentMovie struct {
	ID      string `json:"id,omitempty"`
	IsLive  bool   `json:"isLive,omitempty"`
	SubPath string `json:"subPath,omitempty"`
}

type Status struct {
	LastUpdate   time.Time `json:"lastUpdate,omitempty"`
	CurrentTime  float64   `json:"currentTime,omitempty"`
	PlaybackRate float64   `json:"playbackRate,omitempty"`
	IsPlaying    bool      `json:"isPlaying,omitempty"`
}

func NewStatus() Status {
	return Status{
		CurrentTime:  0,
		PlaybackRate: 1.0,
		LastUpdate:   time.Now(),
	}
}

func (c *Current) UpdateStatus() Status {
	if c.Movie.IsLive {
		c.Status.LastUpdate = time.Now()
		return c.Status
	}
	if c.Status.IsPlaying {
		c.Status.CurrentTime += time.Since(c.Status.LastUpdate).Seconds() * c.Status.PlaybackRate
	}
	c.Status.LastUpdate = time.Now()
	return c.Status
}

func (c *Current) setLiveStatus() Status {
	c.Status.IsPlaying = true
	c.Status.PlaybackRate = 1.0
	c.Status.CurrentTime = 0
	c.Status.LastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetStatus(playing bool, seek, rate, timeDiff float64) Status {
	if c.Movie.IsLive {
		return c.setLiveStatus()
	}
	c.Status.IsPlaying = playing
	c.Status.PlaybackRate = rate
	if playing {
		c.Status.CurrentTime = seek + (timeDiff * rate)
	} else {
		c.Status.CurrentTime = seek
	}
	c.Status.LastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetSeekRate(seek, rate, timeDiff float64) Status {
	if c.Movie.IsLive {
		return c.setLiveStatus()
	}
	if c.Status.IsPlaying {
		c.Status.CurrentTime = seek + (timeDiff * rate)
	} else {
		c.Status.CurrentTime = seek
	}
	c.Status.PlaybackRate = rate
	c.Status.LastUpdate = time.Now()
	return c.Status
}

func (c *Current) SetSeek(seek, timeDiff float64) Status {
	if c.Movie.IsLive {
		return c.setLiveStatus()
	}
	if c.Status.IsPlaying {
		c.Status.CurrentTime = seek + (timeDiff * c.Status.PlaybackRate)
	} else {
		c.Status.CurrentTime = seek
	}
	c.Status.LastUpdate = time.Now()
	return c.Status
}
