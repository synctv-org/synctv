package synccache

import (
	"sync/atomic"
	"time"
)

type entry[V any] struct {
	expiration int64
	value      V
}

func NewEntry[V any](value V, expire time.Duration) *entry[V] {
	return &entry[V]{
		expiration: time.Now().Add(expire).UnixMilli(),
		value:      value,
	}
}

func (e *entry[V]) IsExpired() bool {
	return time.Now().After(time.UnixMilli(atomic.LoadInt64(&e.expiration)))
}

func (e *entry[V]) AddExpiration(d time.Duration) {
	atomic.AddInt64(&e.expiration, int64(d))
}

func (e *entry[V]) SetExpiration(t time.Time) {
	atomic.StoreInt64(&e.expiration, t.UnixMilli())
}
