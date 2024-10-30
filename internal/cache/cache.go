package cache

import (
	"context"
	"sync"
	"time"

	"github.com/zijiren233/gencontainer/refreshcache"
	"golang.org/x/exp/maps"
)

type MapRefreshFunc[T any, A any] func(ctx context.Context, key string, args ...A) (T, error)

type MapCache[T any, A any] struct {
	cache       map[string]*refreshcache.RefreshCache[T, A]
	refreshFunc MapRefreshFunc[T, A]
	maxAge      time.Duration
	lock        sync.RWMutex
}

func newMapCache[T any, A any](refreshFunc MapRefreshFunc[T, A], maxAge time.Duration) *MapCache[T, A] {
	return &MapCache[T, A]{
		cache:       make(map[string]*refreshcache.RefreshCache[T, A]),
		refreshFunc: refreshFunc,
		maxAge:      maxAge,
	}
}

func (b *MapCache[T, A]) Clear() {
	b.lock.Lock()
	defer b.lock.Unlock()
	b.clear()
}

func (b *MapCache[T, A]) clear() {
	maps.Clear(b.cache)
}

func (b *MapCache[T, A]) Delete(key string) {
	b.lock.Lock()
	defer b.lock.Unlock()
	delete(b.cache, key)
}

func (b *MapCache[T, A]) LoadOrStore(ctx context.Context, key string, args ...A) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[key]
	if loaded {
		b.lock.RUnlock()
		return c.Get(ctx, args...)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[key]
	if loaded {
		b.lock.Unlock()
		return c.Get(ctx, args...)
	}
	c = refreshcache.NewRefreshCache[T, A](refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return b.refreshFunc(ctx, key, args...)
	}), b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Get(ctx, args...)
}

func (b *MapCache[T, A]) StoreOrRefresh(ctx context.Context, key string, args ...A) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[key]
	if ok {
		b.lock.RUnlock()
		return c.Refresh(ctx, args...)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[key]
	if ok {
		b.lock.Unlock()
		return c.Refresh(ctx, args...)
	}
	c = refreshcache.NewRefreshCache[T, A](refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return b.refreshFunc(ctx, key, args...)
	}), b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Refresh(ctx, args...)
}

func (b *MapCache[T, A]) LoadCache(key string) (*refreshcache.RefreshCache[T, A], bool) {
	b.lock.RLock()
	c, ok := b.cache[key]
	b.lock.RUnlock()
	return c, ok
}

func (b *MapCache[T, A]) LoadOrNewCache(key string) *refreshcache.RefreshCache[T, A] {
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
	c = refreshcache.NewRefreshCache[T, A](refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return b.refreshFunc(ctx, key, args...)
	}), b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c
}

func (b *MapCache[T, A]) LoadOrStoreWithDynamicFunc(ctx context.Context, key string, refreshFunc MapRefreshFunc[T, A], args ...A) (T, error) {
	b.lock.RLock()
	c, loaded := b.cache[key]
	if loaded {
		b.lock.RUnlock()
		return c.Data().Get(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
			return refreshFunc(ctx, key, args...)
		}), args...)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[key]
	if loaded {
		b.lock.Unlock()
		return c.Data().Get(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
			return refreshFunc(ctx, key, args...)
		}), args...)
	}
	c = refreshcache.NewRefreshCache[T, A](refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return b.refreshFunc(ctx, key, args...)
	}), b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Data().Get(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return refreshFunc(ctx, key, args...)
	}), args...)
}

func (b *MapCache[T, A]) StoreOrRefreshWithDynamicFunc(ctx context.Context, key string, refreshFunc MapRefreshFunc[T, A], args ...A) (T, error) {
	b.lock.RLock()
	c, ok := b.cache[key]
	if ok {
		b.lock.RUnlock()
		return c.Data().Refresh(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
			return refreshFunc(ctx, key, args...)
		}), args...)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[key]
	if ok {
		b.lock.Unlock()
		return c.Data().Refresh(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
			return refreshFunc(ctx, key, args...)
		}), args...)
	}
	c = refreshcache.NewRefreshCache[T, A](refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return b.refreshFunc(ctx, key, args...)
	}), b.maxAge)
	b.cache[key] = c
	b.lock.Unlock()
	return c.Data().Refresh(ctx, refreshcache.RefreshFunc[T, A](func(ctx context.Context, args ...A) (T, error) {
		return refreshFunc(ctx, key, args...)
	}), args...)
}
