package cache

import (
	"context"
	"sync"
	"time"

	"github.com/zijiren233/gencontainer/refreshcache"
	"golang.org/x/exp/maps"
)

type MapRefreshFunc[T any] func(ctx context.Context, id string, args ...any) (T, error)

type MapCache[T any] struct {
	lock        sync.RWMutex
	cache       map[string]*refreshcache.RefreshCache[T]
	refreshFunc MapRefreshFunc[T]
	maxAge      time.Duration
}

func newMapCache[T any](refreshFunc MapRefreshFunc[T], maxAge time.Duration) *MapCache[T] {
	return &MapCache[T]{
		cache:       make(map[string]*refreshcache.RefreshCache[T]),
		refreshFunc: refreshFunc,
		maxAge:      maxAge,
	}
}

func (b *MapCache[T]) Clear() {
	b.lock.Lock()
	defer b.lock.Unlock()
	b.clear()
}

func (b *MapCache[T]) clear() {
	maps.Clear(b.cache)
}

func (b *MapCache[T]) LoadOrStore(ctx context.Context, id string) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[id]
	if loaded {
		b.lock.RUnlock()
		return c.Get(ctx)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[id]
	if loaded {
		b.lock.Unlock()
		return c.Get(ctx)
	}
	c = refreshcache.NewRefreshCache[T](func(ctx context.Context, args ...any) (T, error) {
		return b.refreshFunc(ctx, id, args...)
	}, b.maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Get(ctx)
}

func (b *MapCache[T]) StoreOrRefresh(ctx context.Context, id string) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[id]
	if ok {
		b.lock.RUnlock()
		return c.Refresh(ctx)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[id]
	if ok {
		b.lock.Unlock()
		return c.Refresh(ctx)
	}
	c = refreshcache.NewRefreshCache[T](func(ctx context.Context, args ...any) (T, error) {
		return b.refreshFunc(ctx, id, args...)
	}, b.maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Refresh(ctx)
}

func (b *MapCache[T]) LoadOrStoreWithDynamicFunc(ctx context.Context, id string, refreshFunc MapRefreshFunc[T]) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[id]
	if loaded {
		b.lock.RUnlock()
		return c.Data().Get(ctx, func(ctx context.Context, args ...any) (T, error) {
			return refreshFunc(ctx, id, args...)
		})
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[id]
	if loaded {
		b.lock.Unlock()
		return c.Data().Get(ctx, func(ctx context.Context, args ...any) (T, error) {
			return refreshFunc(ctx, id, args...)
		})
	}
	c = refreshcache.NewRefreshCache[T](func(ctx context.Context, args ...any) (T, error) {
		return b.refreshFunc(ctx, id, args...)
	}, b.maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Data().Get(ctx, func(ctx context.Context, args ...any) (T, error) {
		return refreshFunc(ctx, id, args...)
	})
}

func (b *MapCache[T]) StoreOrRefreshWithDynamicFunc(ctx context.Context, id string, refreshFunc MapRefreshFunc[T]) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[id]
	if ok {
		b.lock.RUnlock()
		return c.Data().Refresh(ctx, func(ctx context.Context, args ...any) (T, error) {
			return refreshFunc(ctx, id, args...)
		})
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[id]
	if ok {
		b.lock.Unlock()
		return c.Data().Refresh(ctx, func(ctx context.Context, args ...any) (T, error) {
			return refreshFunc(ctx, id, args...)
		})
	}
	c = refreshcache.NewRefreshCache[T](func(ctx context.Context, args ...any) (T, error) {
		return b.refreshFunc(ctx, id, args...)
	}, b.maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Data().Refresh(ctx, func(ctx context.Context, args ...any) (T, error) {
		return refreshFunc(ctx, id, args...)
	})
}
