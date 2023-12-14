package cache

import (
	"context"
	"errors"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/vendors/api/alist"
	"github.com/zijiren233/gencontainer/refreshcache"
)

type AlistUserCache = refreshcache.RefreshCache[*AlistUserCacheData, string]

type AlistUserCacheData struct {
	Host  string
	Token string
}

func NewAlistCache(userID string) *AlistUserCache {
	return refreshcache.NewRefreshCache[*AlistUserCacheData](func(ctx context.Context, args ...string) (*AlistUserCacheData, error) {
		var backend string
		if len(args) == 1 {
			backend = args[0]
		}
		return AlistAuthorizationCacheWithUserIDInitFunc(userID, backend)(ctx)
	}, time.Hour*24)
}

func AlistAuthorizationCacheWithConfigInitFunc(host, username, password, backend string) func(ctx context.Context, args ...string) (*AlistUserCacheData, error) {
	return func(ctx context.Context, args ...string) (*AlistUserCacheData, error) {
		cli := vendor.LoadAlistClient(backend)
		if username == "" {
			_, err := cli.Me(ctx, &alist.MeReq{
				Host: host,
			})
			return &AlistUserCacheData{
				Host: host,
			}, err
		} else {
			resp, err := cli.Login(ctx, &alist.LoginReq{
				Host:     host,
				Username: username,
				Password: password,
			})
			if err != nil {
				return nil, err
			}
			return &AlistUserCacheData{
				Host:  host,
				Token: resp.Token,
			}, nil
		}
	}
}

func AlistAuthorizationCacheWithUserIDInitFunc(userID string, backend string) func(ctx context.Context, args ...any) (*AlistUserCacheData, error) {
	return func(ctx context.Context, args ...any) (*AlistUserCacheData, error) {
		v, err := db.GetAlistVendor(userID)
		if err != nil {
			return nil, err
		}

		return AlistAuthorizationCacheWithConfigInitFunc(v.Host, v.Username, v.Password, backend)(ctx)
	}
}

type AlistMovieCache = refreshcache.RefreshCache[*AlistMovieCacheData, string]

func NewAlistMovieCache(user *AlistUserCache, movie *model.Movie) *AlistMovieCache {
	return refreshcache.NewRefreshCache[*AlistMovieCacheData, string](NewAlistMovieCacheInitFunc(user, movie), time.Hour)
}

type AlistMovieCacheData struct {
	URL string
}

func NewAlistMovieCacheInitFunc(user *AlistUserCache, movie *model.Movie) func(ctx context.Context, args ...string) (*AlistMovieCacheData, error) {
	return func(ctx context.Context, args ...string) (*AlistMovieCacheData, error) {
		aucd, err := user.Get(ctx)
		if err != nil {
			return nil, err
		}
		if aucd.Host == "" {
			return nil, errors.New("not bind alist vendor")
		}
		cli := vendor.LoadAlistClient(movie.Base.VendorInfo.Backend)
		fg, err := cli.FsGet(ctx, &alist.FsGetReq{
			Host:     aucd.Host,
			Token:    aucd.Token,
			Path:     movie.Base.VendorInfo.Alist.Path,
			Password: movie.Base.VendorInfo.Alist.Password,
		})
		if err != nil {
			return nil, err
		}

		if fg.IsDir {
			return nil, errors.New("path is dir")
		}

		cache := &AlistMovieCacheData{
			URL: fg.RawUrl,
		}
		if fg.Provider == "AliyundriveOpen" {
			fo, err := cli.FsOther(ctx, &alist.FsOtherReq{
				// Host:     v.Host,
				// Token:    v.Authorization,
				Path:     movie.Base.VendorInfo.Alist.Path,
				Password: movie.Base.VendorInfo.Alist.Password,
				Method:   "video_preview",
			})
			if err != nil {
				return nil, err
			}
			cache.URL = fo.VideoPreviewPlayInfo.LiveTranscodingTaskList[len(fo.VideoPreviewPlayInfo.LiveTranscodingTaskList)-1].Url
		}
		return cache, nil
	}
}
