package refreshcache

import (
	"sync"
	"time"
)

type RefreshCache[T any] struct {
	lock        sync.RWMutex
	last        time.Time
	maxAge      time.Duration
	refreshFunc func() (T, error)
	data        T
}

func NewRefreshCache[T any](refreshFunc func() (T, error), maxAge time.Duration) *RefreshCache[T] {
	if refreshFunc == nil {
		panic("refreshFunc cannot be nil")
	}
	if maxAge <= 0 {
		panic("maxAge must be positive")
	}
	return &RefreshCache[T]{
		refreshFunc: refreshFunc,
		maxAge:      maxAge,
	}
}

func (r *RefreshCache[T]) Get() (data T, err error) {
	r.lock.RLock()
	if time.Since(r.last) < r.maxAge {
		r.lock.RUnlock()
		return r.data, nil
	}
	r.lock.RUnlock()
	r.lock.Lock()
	defer r.lock.Unlock()
	if time.Since(r.last) < r.maxAge {
		return r.data, nil
	}
	defer func() {
		if err == nil {
			r.data = data
			r.last = time.Now()
		}
	}()
	return r.refreshFunc()
}
