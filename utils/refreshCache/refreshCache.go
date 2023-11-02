package refreshcache

import (
	"sync"
	"sync/atomic"
	"time"
)

type RefreshCache[T any] struct {
	lock        sync.Mutex
	last        int64
	maxAge      int64
	refreshFunc func() (T, error)
	data        atomic.Pointer[T]
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
		maxAge:      int64(maxAge),
	}
}

func (r *RefreshCache[T]) Get() (data T, err error) {
	if time.Now().UnixNano()-atomic.LoadInt64(&r.last) < r.maxAge {
		return *r.data.Load(), nil
	}
	r.lock.Lock()
	defer r.lock.Unlock()
	if time.Now().UnixNano()-r.last < r.maxAge {
		return *r.data.Load(), nil
	}
	defer func() {
		if err == nil {
			r.data.Store(&data)
			atomic.StoreInt64(&r.last, time.Now().UnixNano())
		}
	}()
	return r.refreshFunc()
}

func (r *RefreshCache[T]) Refresh() (data T, err error) {
	r.lock.Lock()
	defer r.lock.Unlock()
	defer func() {
		if err == nil {
			r.data.Store(&data)
			atomic.StoreInt64(&r.last, time.Now().UnixNano())
		}
	}()
	return r.refreshFunc()
}
