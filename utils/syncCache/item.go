package synccache

import (
	"sync/atomic"
	"time"
)

type Entry[V any] struct {
	expiration int64
	value      V
}

func NewEntry[V any](value V, expire time.Duration) *Entry[V] {
	return &Entry[V]{
		expiration: time.Now().Add(expire).UnixMilli(),
		value:      value,
	}
}

func (e *Entry[V]) Value() V {
	return e.value
}

func (e *Entry[V]) IsExpired() bool {
	return time.Now().After(time.UnixMilli(atomic.LoadInt64(&e.expiration)))
}

func (e *Entry[V]) AddExpiration(d time.Duration) {
	atomic.AddInt64(&e.expiration, int64(d))
}

func (e *Entry[V]) SetExpiration(t time.Time) {
	atomic.StoreInt64(&e.expiration, t.UnixMilli())
}
