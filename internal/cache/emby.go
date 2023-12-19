package cache

import (
	"context"
	"errors"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/zijiren233/gencontainer/refreshcache"
)

type EmbyUserCache = refreshcache.RefreshCache[*EmbyUserCacheData, struct{}]

type EmbyUserCacheData struct {
	Host    string
	ApiKey  string
	Backend string
}

func NewEmbyUserCache(userID string) *EmbyUserCache {
	f := EmbyAuthorizationCacheWithUserIDInitFunc(userID)
	return refreshcache.NewRefreshCache(func(ctx context.Context, args ...struct{}) (*EmbyUserCacheData, error) {
		return f(ctx)
	}, 0)
}

func EmbyAuthorizationCacheWithUserIDInitFunc(userID string) func(ctx context.Context, args ...struct{}) (*EmbyUserCacheData, error) {
	return func(ctx context.Context, args ...struct{}) (*EmbyUserCacheData, error) {
		v, err := db.GetEmbyVendor(userID)
		if err != nil {
			if errors.Is(err, db.ErrNotFound("vendor")) {
				return nil, errors.New("emby not logged in")
			}
			return nil, err
		}
		if v.ApiKey == "" || v.Host == "" {
			return nil, errors.New("emby not logged in")
		}
		return &EmbyUserCacheData{
			Host:    v.Host,
			ApiKey:  v.ApiKey,
			Backend: v.Backend,
		}, nil
	}
}
