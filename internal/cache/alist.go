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

type AlistUserCache = refreshcache.RefreshCache[*AlistUserCacheData, struct{}]

type AlistUserCacheData struct {
	Host    string
	Token   string
	Backend string
}

func NewAlistUserCache(userID string) *AlistUserCache {
	f := AlistAuthorizationCacheWithUserIDInitFunc(userID)
	return refreshcache.NewRefreshCache(func(ctx context.Context, args ...struct{}) (*AlistUserCacheData, error) {
		return f(ctx)
	}, 0)
}

func AlistAuthorizationCacheWithUserIDInitFunc(userID string) func(ctx context.Context, args ...struct{}) (*AlistUserCacheData, error) {
	return func(ctx context.Context, args ...struct{}) (*AlistUserCacheData, error) {
		v, err := db.GetAlistVendor(userID)
		if err != nil {
			return nil, err
		}
		return AlistAuthorizationCacheWithConfigInitFunc(v)(ctx)
	}
}

func AlistAuthorizationCacheWithConfigInitFunc(v *model.AlistVendor) func(ctx context.Context, args ...struct{}) (*AlistUserCacheData, error) {
	return func(ctx context.Context, args ...struct{}) (*AlistUserCacheData, error) {
		cli := vendor.LoadAlistClient(v.Backend)
		if v.Username == "" {
			_, err := cli.Me(ctx, &alist.MeReq{
				Host: v.Host,
			})
			return &AlistUserCacheData{
				Host:    v.Host,
				Backend: v.Backend,
			}, err
		} else {
			resp, err := cli.Login(ctx, &alist.LoginReq{
				Host:     v.Host,
				Username: v.UserID,
				Password: string(v.HashedPassword),
				Hashed:   true,
			})
			if err != nil {
				return nil, err
			}
			return &AlistUserCacheData{
				Host:    v.Host,
				Token:   resp.Token,
				Backend: v.Backend,
			}, nil
		}
	}
}

type AlistMovieCache = refreshcache.RefreshCache[*AlistMovieCacheData, *AlistUserCache]

func NewAlistMovieCache(movie *model.Movie) *AlistMovieCache {
	return refreshcache.NewRefreshCache(NewAlistMovieCacheInitFunc(movie), time.Hour)
}

type AlistMovieCacheData struct {
	URL string
}

func NewAlistMovieCacheInitFunc(movie *model.Movie) func(ctx context.Context, args ...*AlistUserCache) (*AlistMovieCacheData, error) {
	return func(ctx context.Context, args ...*AlistUserCache) (*AlistMovieCacheData, error) {
		if len(args) == 0 {
			return nil, errors.New("need alist user cache")
		}
		aucd, err := args[0].Get(ctx)
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
				Host:     aucd.Host,
				Token:    aucd.Token,
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
