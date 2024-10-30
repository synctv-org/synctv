package cache

import (
	"context"
	"sync"
	"time"

	"github.com/zijiren233/gencontainer/refreshcache0"
	"golang.org/x/exp/maps"
)

type MapRefreshFunc0[T any] func(ctx context.Context, key string) (T, error)

type MapCache0[T any] struct {
	cache       map[string]*refreshcache0.RefreshCache[T]
	refreshFunc MapRefreshFunc0[T]
	maxAge      time.Duration
	lock        sync.RWMutex
}

func newMapCache0[T any](refreshFunc MapRefreshFunc0[T], maxAge time.Duration) *MapCache0[T] {
	return &MapCache0[T]{
		cache:       make(map[string]*refreshcache0.RefreshCache[T]),
		refreshFunc: refreshFunc,
		maxAge:      maxAge,
	}
}

func (b *MapCache0[T]) Clear() {
	b.lock.Lock()
	defer b.lock.Unlock()
	b.clear()
}

func (b *MapCache0[T]) clear() {
	maps.Clear(b.cache)
}

func (b *MapCache0[T]) Delete(key string) {
	b.lock.Lock()
	defer b.lock.Unlock()
	delete(b.cache, key)
}

func (b *MapCache0[T]) LoadOrStore(ctx context.Context, key string) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[key]
	if loaded {
		b.lock.RUnlock()
		return c.Get(ctx)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[key]
	if loaded {
		b.lock.Unlock()
		return c.Get(ctx)
	}
	c = refreshcache0.NewRefreshCache[T](func(ctx context.Context) (T, error) {
		return b.refreshFunc(ctx, key)
	}, b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Get(ctx)
}

func (b *MapCache0[T]) StoreOrRefresh(ctx context.Context, key string) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[key]
	if ok {
		b.lock.RUnlock()
		return c.Refresh(ctx)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[key]
	if ok {
		b.lock.Unlock()
		return c.Refresh(ctx)
	}
	c = refreshcache0.NewRefreshCache[T](func(ctx context.Context) (T, error) {
		return b.refreshFunc(ctx, key)
	}, b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Refresh(ctx)
}

func (b *MapCache0[T]) LoadCache(key string) (*refreshcache0.RefreshCache[T], bool) {
	b.lock.RLock()
	c, ok := b.cache[key]
	b.lock.RUnlock()
	return c, ok
}

func (b *MapCache0[T]) LoadOrNewCache(key string) *refreshcache0.RefreshCache[T] {
	b.lock.RLock()
	c, ok := b.cache[key]
	if ok {
		b.lock.RUnlock()
		return c
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[key]
	if ok {
		b.lock.Unlock()
		return c
	}
	c = refreshcache0.NewRefreshCache[T](func(ctx context.Context) (T, error) {
		return b.refreshFunc(ctx, key)
	}, b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c
}

func (b *MapCache0[T]) LoadOrStoreWithDynamicFunc(ctx context.Context, key string, refreshFunc MapRefreshFunc0[T]) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[key]
	if loaded {
		b.lock.RUnlock()
		return c.Data().Get(ctx, func(ctx context.Context) (T, error) {
			return refreshFunc(ctx, key)
		})
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[key]
	if loaded {
		b.lock.Unlock()
		return c.Data().Get(ctx, func(ctx context.Context) (T, error) {
			return refreshFunc(ctx, key)
		})
	}
	c = refreshcache0.NewRefreshCache[T](func(ctx context.Context) (T, error) {
		return b.refreshFunc(ctx, key)
	}, b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Data().Get(ctx, func(ctx context.Context) (T, error) {
		return refreshFunc(ctx, key)
	})
}

func (b *MapCache0[T]) StoreOrRefreshWithDynamicFunc(ctx context.Context, key string, refreshFunc MapRefreshFunc0[T]) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[key]
	if ok {
		b.lock.RUnlock()
		return c.Data().Refresh(ctx, func(ctx context.Context) (T, error) {
			return refreshFunc(ctx, key)
		})
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[key]
	if ok {
		b.lock.Unlock()
		return c.Data().Refresh(ctx, func(ctx context.Context) (T, error) {
			return refreshFunc(ctx, key)
		})
	}
	c = refreshcache0.NewRefreshCache[T](func(ctx context.Context) (T, error) {
		return b.refreshFunc(ctx, key)
	}, b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Data().Refresh(ctx, func(ctx context.Context) (T, error) {
		return refreshFunc(ctx, key)
	})
}
