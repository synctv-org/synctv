package op

import (
	"sync"
	"time"

	"github.com/zijiren233/gencontainer/refreshcache"
	"golang.org/x/exp/maps"
)

type Cache struct {
	lock  sync.RWMutex
	cache map[string]*refreshcache.RefreshCache[any]
}

func newBaseCache() *Cache {
	return &Cache{
		cache: make(map[string]*refreshcache.RefreshCache[any]),
	}
}

func (b *Cache) Clear() {
	b.lock.Lock()
	defer b.lock.Unlock()
	b.clear()
}

func (b *Cache) clear() {
	maps.Clear(b.cache)
}

func (b *Cache) LoadOrStore(id string, refreshFunc func() (any, error), maxAge time.Duration) (any, error) {
	b.lock.RLock()
	c, loaded := b.cache[id]
	if loaded {
		b.lock.RUnlock()
		return c.Get()
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[id]
	if loaded {
		b.lock.Unlock()
		return c.Get()
	}
	c = refreshcache.NewRefreshCache[any](refreshFunc, maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Get()
}

func (b *Cache) StoreOrRefresh(id string, refreshFunc func() (any, error), maxAge time.Duration) (any, error) {
	b.lock.RLock()
	c, ok := b.cache[id]
	if ok {
		b.lock.RUnlock()
		return c.Refresh()
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[id]
	if ok {
		b.lock.Unlock()
		return c.Refresh()
	}
	c = refreshcache.NewRefreshCache[any](refreshFunc, maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Refresh()
}

func (b *Cache) LoadOrStoreWithDynamicFunc(id string, refreshFunc func() (any, error), maxAge time.Duration) (any, error) {
	b.lock.RLock()
	c, loaded := b.cache[id]
	if loaded {
		b.lock.RUnlock()
		return c.Data().Get(refreshFunc)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, loaded = b.cache[id]
	if loaded {
		b.lock.Unlock()
		return c.Data().Get(refreshFunc)
	}
	c = refreshcache.NewRefreshCache[any](refreshFunc, maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Data().Get(refreshFunc)
}

func (b *Cache) StoreOrRefreshWithDynamicFunc(id string, refreshFunc func() (any, error), maxAge time.Duration) (any, error) {
	b.lock.RLock()
	c, ok := b.cache[id]
	if ok {
		b.lock.RUnlock()
		return c.Data().Refresh(refreshFunc)
	}
	b.lock.RUnlock()
	b.lock.Lock()
	c, ok = b.cache[id]
	if ok {
		b.lock.Unlock()
		return c.Data().Refresh(refreshFunc)
	}
	c = refreshcache.NewRefreshCache[any](refreshFunc, maxAge)
	b.cache[id] = c
	b.lock.Unlock()
	return c.Data().Refresh(refreshFunc)
}
