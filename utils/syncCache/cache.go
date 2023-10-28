package synccache

import (
	"time"

	"github.com/zijiren233/gencontainer/rwmap"
)

type SyncCache[K comparable, V any] struct {
	cache           rwmap.RWMap[K, *Entry[V]]
	deletedCallback func(v V)
	ticker          *time.Ticker
}

type SyncCacheConfig[K comparable, V any] func(sc *SyncCache[K, V])

func WithDeletedCallback[K comparable, V any](callback func(v V)) SyncCacheConfig[K, V] {
	return func(sc *SyncCache[K, V]) {
		sc.deletedCallback = callback
	}
}

func NewSyncCache[K comparable, V any](trimTime time.Duration, conf ...SyncCacheConfig[K, V]) *SyncCache[K, V] {
	sc := &SyncCache[K, V]{
		ticker: time.NewTicker(trimTime),
	}
	for _, c := range conf {
		c(sc)
	}
	go func() {
		for range sc.ticker.C {
			sc.trim()
		}
	}()
	return sc
}

func (sc *SyncCache[K, V]) trim() {
	sc.cache.Range(func(key K, value *Entry[V]) bool {
		if value.IsExpired() {
			sc.CompareAndDelete(key, value)
		}
		return true
	})
}

func (sc *SyncCache[K, V]) Store(key K, value V, expire time.Duration) {
	sc.LoadOrStore(key, value, expire)
}

func (sc *SyncCache[K, V]) Load(key K) (value *Entry[V], loaded bool) {
	e, ok := sc.cache.Load(key)
	if ok && !e.IsExpired() {
		return e, ok
	}
	sc.CompareAndDelete(key, e)
	return
}

func (sc *SyncCache[K, V]) LoadOrStore(key K, value V, expire time.Duration) (actual *Entry[V], loaded bool) {
	e, loaded := sc.cache.LoadOrStore(key, NewEntry[V](value, expire))
	if loaded && e.IsExpired() {
		sc.CompareAndDelete(key, e)
		return sc.LoadOrStore(key, value, expire)
	}
	return e, loaded
}

func (sc *SyncCache[K, V]) Delete(key K) {
	sc.LoadAndDelete(key)
}

func (sc *SyncCache[K, V]) LoadAndDelete(key K) (value *Entry[V], loaded bool) {
	e, loaded := sc.cache.LoadAndDelete(key)
	if loaded && !e.IsExpired() {
		return e, loaded
	}
	return
}

func (sc *SyncCache[K, V]) CompareAndDelete(key K, oldEntry *Entry[V]) (success bool) {
	b := sc.cache.CompareAndDelete(key, oldEntry)
	if b && sc.deletedCallback != nil {
		sc.deletedCallback(oldEntry.value)
	}
	return b
}

func (sc *SyncCache[K, V]) Clear() {
	sc.cache.Clear()
}

func (sc *SyncCache[K, V]) Range(f func(key K, value *Entry[V]) bool) {
	sc.cache.Range(func(key K, value *Entry[V]) bool {
		if !value.IsExpired() {
			return f(key, value)
		}
		sc.cache.CompareAndDelete(key, value)
		return true
	})
}
