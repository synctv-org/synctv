package synccache

import "time"

type entry[V any] struct {
	expiration time.Time
	value      V
}

func (e *entry[V]) IsExpired() bool {
	return time.Now().After(e.expiration)
}
