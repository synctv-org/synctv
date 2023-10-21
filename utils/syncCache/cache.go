package synccache

import (
	"time"

	"github.com/zijiren233/gencontainer/rwmap"
)

type SyncCache[K comparable, V any] struct {
	cache           rwmap.RWMap[K, *entry[V]]
	deletedCallback func(v V)
	ticker          *time.Ticker
}

type SyncCacheConfig[K comparable, V any] func(sc *SyncCache[K, V])

func WithDeletedCallback[K comparable, V any](callback func(v V)) SyncCacheConfig[K, V] {
	return func(sc *SyncCache[K, V]) {
		sc.deletedCallback = callback
	}
}

func NewSyncCache[K comparable, V any](trimTime time.Duration) *SyncCache[K, V] {
	sc := &SyncCache[K, V]{
		ticker: time.NewTicker(trimTime),
	}
	go func() {
		for range sc.ticker.C {
			sc.trim()
		}
	}()
	return sc
}

func (sc *SyncCache[K, V]) Releases() {
	sc.ticker.Stop()
	sc.cache.Clear()
}

func (sc *SyncCache[K, V]) trim() {
	sc.cache.Range(func(key K, value *entry[V]) bool {
		if value.IsExpired() {
			e, loaded := sc.cache.LoadAndDelete(key)
			if loaded && sc.deletedCallback != nil {
				sc.deletedCallback(e.value)
			}
		}
		return true
	})
}

func (sc *SyncCache[K, V]) Store(key K, value V, expire time.Duration) {
	sc.LoadOrStore(key, value, expire)
}

func (sc *SyncCache[K, V]) Load(key K) (value V, loaded bool) {
	e, ok := sc.cache.Load(key)
	if ok && !e.IsExpired() {
		return e.value, ok
	}
	return
}

func (sc *SyncCache[K, V]) LoadOrStore(key K, value V, expire time.Duration) (actual V, loaded bool) {
	e, loaded := sc.cache.LoadOrStore(key, &entry[V]{
		expiration: time.Now().Add(expire),
		value:      value,
	})
	if e.IsExpired() {
		sc.cache.Store(key, &entry[V]{
			expiration: time.Now().Add(expire),
			value:      value,
		})
		return value, false
	}
	return e.value, loaded
}

func (sc *SyncCache[K, V]) Delete(key K) {
	sc.LoadAndDelete(key)
}

func (sc *SyncCache[K, V]) LoadAndDelete(key K) (value V, loaded bool) {
	e, loaded := sc.cache.LoadAndDelete(key)
	if loaded && !e.IsExpired() {
		return e.value, loaded
	}
	return
}

func (sc *SyncCache[K, V]) Clear() {
	sc.cache.Clear()
}
