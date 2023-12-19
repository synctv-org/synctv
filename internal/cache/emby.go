package cache

import (
	"context"
	"errors"
	"fmt"
	"net/url"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/vendors/api/emby"
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

type EmbySource struct {
	URLs []struct {
		URL  string
		Name string
	}
	// TODO: cache subtitles
	Subtitles []struct {
		URL  string
		Name string
	}
}

type EmbyMovieCacheData struct {
	Sources []EmbySource
}

type EmbyMovieCache = refreshcache.RefreshCache[*EmbyMovieCacheData, *EmbyUserCache]

func NewEmbyMovieCache(movie *model.Movie) *EmbyMovieCache {
	return refreshcache.NewRefreshCache(NewEmbyMovieCacheInitFunc(movie), 0)
}

func NewEmbyMovieCacheInitFunc(movie *model.Movie) func(ctx context.Context, args ...*EmbyUserCache) (*EmbyMovieCacheData, error) {
	return func(ctx context.Context, args ...*EmbyUserCache) (*EmbyMovieCacheData, error) {
		if len(args) == 0 {
			return nil, errors.New("need emby user cache")
		}
		aucd, err := args[0].Get(ctx)
		if err != nil {
			return nil, err
		}
		if aucd.Host == "" || aucd.ApiKey == "" {
			return nil, errors.New("not bind emby vendor")
		}
		u, err := url.Parse(aucd.Host)
		if err != nil {
			return nil, err
		}
		cli := vendor.LoadEmbyClient(aucd.Backend)
		data, err := cli.GetItem(ctx, &emby.GetItemReq{
			Host:   aucd.Host,
			Token:  aucd.ApiKey,
			ItemId: movie.Base.VendorInfo.Emby.Path,
		})
		if err != nil {
			return nil, err
		}
		if data.IsFolder {
			return nil, errors.New("path is dir")
		}
		var resp EmbyMovieCacheData = EmbyMovieCacheData{
			Sources: make([]EmbySource, len(data.MediaSourceInfo)),
		}
		for i, v := range data.MediaSourceInfo {
			result, err := url.JoinPath("emby", "Videos", data.Id, fmt.Sprintf("stream.%s", v.Container))
			if err != nil {
				return nil, err
			}
			u.Path = result
			query := url.Values{}
			query.Set("api_key", aucd.ApiKey)
			query.Set("Static", "true")
			query.Set("MediaSourceId", v.Id)
			u.RawQuery = query.Encode()
			resp.Sources[i].URLs = append(resp.Sources[i].URLs, struct {
				URL  string
				Name string
			}{
				URL:  u.String(),
				Name: v.Name,
			})
			for _, msi := range v.MediaStreamInfo {
				switch msi.Type {
				case "Subtitle":
					result, err = url.JoinPath("emby", "Videos", data.Id, v.Id, "Subtitles", fmt.Sprintf("%d", msi.Index), "Stream.srt")
					if err != nil {
						return nil, err
					}
					u.Path = result
					u.RawQuery = ""
					resp.Sources[i].Subtitles = append(resp.Sources[i].Subtitles, struct {
						URL  string
						Name string
					}{
						URL:  u.String(),
						Name: msi.DisplayTitle,
					})
				}
			}
		}
		return &resp, nil
	}
}
