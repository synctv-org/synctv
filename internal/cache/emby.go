package cache

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/emby"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/go-uhc"
)

type EmbyUserCache = MapCache[*EmbyUserCacheData, struct{}]

type EmbyUserCacheData struct {
	Host     string
	ServerID string
	ApiKey   string
	UserID   string
	Backend  string
}

func NewEmbyUserCache(userID string) *EmbyUserCache {
	return newMapCache(func(ctx context.Context, key string, args ...struct{}) (*EmbyUserCacheData, error) {
		return EmbyAuthorizationCacheWithUserIDInitFunc(userID, key)
	}, -1)
}

func EmbyAuthorizationCacheWithUserIDInitFunc(userID, serverID string) (*EmbyUserCacheData, error) {
	if serverID == "" {
		return nil, errors.New("serverID is required")
	}
	v, err := db.GetEmbyVendor(userID, serverID)
	if err != nil {
		return nil, err
	}
	if v.ApiKey == "" || v.Host == "" {
		return nil, db.ErrNotFound("vendor")
	}
	return &EmbyUserCacheData{
		Host:     v.Host,
		ServerID: v.ServerID,
		ApiKey:   v.ApiKey,
		UserID:   v.EmbyUserID,
		Backend:  v.Backend,
	}, nil
}

type EmbySource struct {
	URL         string
	IsTranscode bool
	Name        string
	Subtitles   []struct {
		URL   string
		Type  string
		Name  string
		Cache *refreshcache.RefreshCache[[]byte, struct{}]
	}
}

type EmbyMovieCacheData struct {
	Sources            []EmbySource
	TranscodeSessionID string
}

type EmbyMovieCache = refreshcache.RefreshCache[*EmbyMovieCacheData, *EmbyUserCache]

func NewEmbyMovieCache(movie *model.Movie) *EmbyMovieCache {
	cache := refreshcache.NewRefreshCache(NewEmbyMovieCacheInitFunc(movie), 0)
	cache.SetClearFunc(NewEmbyMovieClearCacheFunc(movie))
	return cache
}

func NewEmbyMovieClearCacheFunc(movie *model.Movie) func(ctx context.Context, args ...*EmbyUserCache) error {
	return func(ctx context.Context, args ...*MapCache[*EmbyUserCacheData, struct{}]) error {
		if !movie.Base.VendorInfo.Emby.Transcode {
			return nil
		}

		serverID, _, err := model.GetEmbyServerIdFromPath(movie.Base.VendorInfo.Emby.Path)
		if err != nil {
			return err
		}

		oldVal, ok := ctx.Value(refreshcache.OldValKey).(*EmbyMovieCacheData)
		if !ok {
			return nil
		}

		aucd, err := args[0].LoadOrStore(ctx, serverID)
		if err != nil {
			return err
		}
		if aucd.Host == "" || aucd.ApiKey == "" {
			return errors.New("not bind emby vendor")
		}
		cli := vendor.LoadEmbyClient(aucd.Backend)
		_, err = cli.DeleteActiveEncodeings(ctx, &emby.DeleteActiveEncodeingsReq{
			Host:          aucd.Host,
			Token:         aucd.ApiKey,
			PalySessionId: oldVal.TranscodeSessionID,
		})
		if err != nil {
			log.Errorf("delete active encodeings: %v", err)
		}
		return nil
	}
}

func NewEmbyMovieCacheInitFunc(movie *model.Movie) func(ctx context.Context, args ...*EmbyUserCache) (*EmbyMovieCacheData, error) {
	return func(ctx context.Context, args ...*EmbyUserCache) (*EmbyMovieCacheData, error) {
		if len(args) == 0 {
			return nil, errors.New("need emby user cache")
		}

		var (
			serverID string
			err      error
			truePath string
		)
		serverID, truePath, err = model.GetEmbyServerIdFromPath(movie.Base.VendorInfo.Emby.Path)
		if err != nil {
			return nil, err
		}

		aucd, err := args[0].LoadOrStore(ctx, serverID)
		if err != nil {
			return nil, err
		}
		if aucd.Host == "" || aucd.ApiKey == "" {
			return nil, errors.New("not bind emby vendor")
		}
		cli := vendor.LoadEmbyClient(aucd.Backend)
		data, err := cli.PlaybackInfo(ctx, &emby.PlaybackInfoReq{
			Host:   aucd.Host,
			Token:  aucd.ApiKey,
			UserId: aucd.UserID,
			ItemId: truePath,
		})
		if err != nil {
			return nil, fmt.Errorf("playback info: %w", err)
		}
		var resp EmbyMovieCacheData = EmbyMovieCacheData{
			Sources:            make([]EmbySource, len(data.MediaSourceInfo)),
			TranscodeSessionID: data.PlaySessionID,
		}
		u, err := url.Parse(aucd.Host)
		if err != nil {
			return nil, err
		}
		for i, v := range data.MediaSourceInfo {
			if movie.Base.VendorInfo.Emby.Transcode && v.TranscodingUrl != "" {
				resp.Sources[i].URL = fmt.Sprintf("%s/emby%s", aucd.Host, v.TranscodingUrl)
				resp.Sources[i].IsTranscode = true
				resp.Sources[i].Name = v.Name
			} else if v.DirectPlayUrl != "" {
				resp.Sources[i].URL = fmt.Sprintf("%s/emby%s", aucd.Host, v.DirectPlayUrl)
				resp.Sources[i].IsTranscode = false
				resp.Sources[i].Name = v.Name
			} else {
				if v.Container == "" {
					continue
				}
				result, err := url.JoinPath("emby", "Videos", truePath, fmt.Sprintf("stream.%s", v.Container))
				if err != nil {
					return nil, err
				}
				u.Path = result
				query := url.Values{}
				query.Set("api_key", aucd.ApiKey)
				query.Set("Static", "true")
				query.Set("MediaSourceId", v.Id)
				u.RawQuery = query.Encode()
				resp.Sources[i].URL = u.String()
				resp.Sources[i].Name = v.Name
			}
			for _, msi := range v.MediaStreamInfo {
				switch msi.Type {
				case "Subtitle":
					subtutleType := "srt"
					result, err := url.JoinPath("emby", "Videos", truePath, v.Id, "Subtitles", fmt.Sprintf("%d", msi.Index), fmt.Sprintf("Stream.%s", subtutleType))
					if err != nil {
						return nil, err
					}
					u.Path = result
					u.RawQuery = ""
					url := u.String()
					name := msi.DisplayTitle
					if name == "" {
						if msi.Title != "" {
							name = msi.Title
						} else {
							name = msi.DisplayLanguage
						}
					}
					resp.Sources[i].Subtitles = append(resp.Sources[i].Subtitles, struct {
						URL   string
						Type  string
						Name  string
						Cache *refreshcache.RefreshCache[[]byte, struct{}]
					}{
						URL:   url,
						Type:  subtutleType,
						Name:  name,
						Cache: refreshcache.NewRefreshCache(newEmbySubtitleCacheInitFunc(url), 0),
					})
				}
			}
		}
		return &resp, nil
	}
}

func newEmbySubtitleCacheInitFunc(url string) func(ctx context.Context, args ...struct{}) ([]byte, error) {
	return func(ctx context.Context, args ...struct{}) ([]byte, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
		if err != nil {
			return nil, err
		}
		req.Header.Set("User-Agent", utils.UA)
		req.Header.Set("Referer", req.URL.Host)
		resp, err := uhc.Do(req)
		if err != nil {
			return nil, err
		}
		defer resp.Body.Close()
		if resp.StatusCode != http.StatusOK {
			return nil, errors.New("bad status code")
		}
		return io.ReadAll(resp.Body)
	}
}
