package cache

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"strconv"

	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/emby"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/gencontainer/refreshcache0"
	"github.com/zijiren233/gencontainer/refreshcache1"
	"github.com/zijiren233/go-uhc"
)

type EmbyUserCache = MapCache0[*EmbyUserCacheData]

type EmbyUserCacheData struct {
	Host     string
	ServerID string
	APIKey   string
	UserID   string
	Backend  string
}

func NewEmbyUserCache(userID string) *EmbyUserCache {
	return newMapCache0(func(_ context.Context, key string) (*EmbyUserCacheData, error) {
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
	if v.APIKey == "" || v.Host == "" {
		return nil, db.NotFoundError(db.ErrVendorNotFound)
	}
	return &EmbyUserCacheData{
		Host:     v.Host,
		ServerID: v.ServerID,
		APIKey:   v.APIKey,
		UserID:   v.EmbyUserID,
		Backend:  v.Backend,
	}, nil
}

type EmbySource struct {
	URL         string
	Name        string
	Subtitles   []*EmbySubtitleCache
	IsTranscode bool
}

type EmbySubtitleCache struct {
	Cache *refreshcache0.RefreshCache[[]byte]
	URL   string
	Type  string
	Name  string
}

type EmbyMovieCacheData struct {
	TranscodeSessionID string
	Sources            []EmbySource
}

type EmbyMovieCache = refreshcache1.RefreshCache[*EmbyMovieCacheData, *EmbyUserCache]

func NewEmbyMovieCache(movie *model.Movie, subPath string) *EmbyMovieCache {
	cache := refreshcache1.NewRefreshCache(NewEmbyMovieCacheInitFunc(movie, subPath), -1)
	cache.SetClearFunc(NewEmbyMovieClearCacheFunc(movie, subPath))
	return cache
}

func NewEmbyMovieClearCacheFunc(
	movie *model.Movie,
	_ string,
) func(ctx context.Context, args *EmbyUserCache) error {
	return func(ctx context.Context, args *EmbyUserCache) error {
		if !movie.VendorInfo.Emby.Transcode {
			return nil
		}
		if args == nil {
			return errors.New("need emby user cache")
		}

		serverID, err := movie.VendorInfo.Emby.ServerID()
		if err != nil {
			return err
		}

		oldVal, ok := ctx.Value(refreshcache.OldValKey).(*EmbyMovieCacheData)
		if !ok {
			return nil
		}

		aucd, err := args.LoadOrStore(ctx, serverID)
		if err != nil {
			return err
		}
		if aucd.Host == "" || aucd.APIKey == "" {
			return errors.New("not bind emby vendor")
		}
		cli := vendor.LoadEmbyClient(aucd.Backend)
		_, err = cli.DeleteActiveEncodeings(ctx, &emby.DeleteActiveEncodeingsReq{
			Host:          aucd.Host,
			Token:         aucd.APIKey,
			PalySessionId: oldVal.TranscodeSessionID,
		})
		if err != nil {
			log.Errorf("delete active encodeings: %v", err)
		}
		return nil
	}
}

func NewEmbyMovieCacheInitFunc(
	movie *model.Movie,
	subPath string,
) func(ctx context.Context, args *EmbyUserCache) (*EmbyMovieCacheData, error) {
	return func(ctx context.Context, args *EmbyUserCache) (*EmbyMovieCacheData, error) {
		if err := validateEmbyArgs(args, movie, subPath); err != nil {
			return nil, err
		}

		serverID, truePath, err := getEmbyServerIDAndPath(movie, subPath)
		if err != nil {
			return nil, err
		}

		aucd, err := args.LoadOrStore(ctx, serverID)
		if err != nil {
			return nil, err
		}
		if aucd.Host == "" || aucd.APIKey == "" {
			return nil, errors.New("not bind emby vendor")
		}

		data, err := getPlaybackInfo(ctx, aucd, truePath)
		if err != nil {
			return nil, err
		}

		resp := &EmbyMovieCacheData{
			Sources:            make([]EmbySource, len(data.GetMediaSourceInfo())),
			TranscodeSessionID: data.GetPlaySessionID(),
		}

		u, err := url.Parse(aucd.Host)
		if err != nil {
			return nil, err
		}

		for i, v := range data.GetMediaSourceInfo() {
			source, err := processMediaSource(v, movie, aucd, truePath, u)
			if err != nil {
				return nil, err
			}
			if source != nil {
				resp.Sources[i] = *source
				resp.Sources[i].Subtitles = processEmbySubtitles(v, truePath, u)
			}
		}

		return resp, nil
	}
}

func validateEmbyArgs(args *EmbyUserCache, movie *model.Movie, subPath string) error {
	if args == nil {
		return errors.New("need emby user cache")
	}
	if movie.IsFolder && subPath == "" {
		return errors.New("sub path is empty")
	}
	return nil
}

func getEmbyServerIDAndPath(movie *model.Movie, subPath string) (string, string, error) {
	serverID, truePath, err := movie.VendorInfo.Emby.ServerIDAndFilePath()
	if err != nil {
		return "", "", err
	}
	if movie.IsFolder {
		truePath = subPath
	}
	return serverID, truePath, nil
}

func getPlaybackInfo(
	ctx context.Context,
	aucd *EmbyUserCacheData,
	truePath string,
) (*emby.PlaybackInfoResp, error) {
	cli := vendor.LoadEmbyClient(aucd.Backend)
	data, err := cli.PlaybackInfo(ctx, &emby.PlaybackInfoReq{
		Host:   aucd.Host,
		Token:  aucd.APIKey,
		UserId: aucd.UserID,
		ItemId: truePath,
	})
	if err != nil {
		return nil, fmt.Errorf("playback info: %w", err)
	}
	return data, nil
}

func processMediaSource(
	v *emby.MediaSourceInfo,
	_ *model.Movie,
	aucd *EmbyUserCacheData,
	truePath string,
	u *url.URL,
) (*EmbySource, error) {
	source := &EmbySource{Name: v.GetName()}

	switch {
	case v.GetTranscodingUrl() != "":
		source.URL = fmt.Sprintf("%s/emby%s", aucd.Host, v.GetTranscodingUrl())
		source.IsTranscode = true
	case v.GetDirectPlayUrl() != "":
		source.URL = fmt.Sprintf("%s/emby%s", aucd.Host, v.GetDirectPlayUrl())
		source.IsTranscode = false
	default:
		if v.GetContainer() == "" {
			return nil, nil
		}
		result, err := url.JoinPath("emby", "Videos", truePath, "stream."+v.GetContainer())
		if err != nil {
			return nil, err
		}
		u.Path = result
		query := url.Values{}
		query.Set("api_key", aucd.APIKey)
		query.Set("Static", "true")
		query.Set("MediaSourceId", v.GetId())
		u.RawQuery = query.Encode()
		source.URL = u.String()
	}

	return source, nil
}

func processEmbySubtitles(
	v *emby.MediaSourceInfo,
	truePath string,
	u *url.URL,
) []*EmbySubtitleCache {
	subtitles := make([]*EmbySubtitleCache, 0, len(v.GetMediaStreamInfo()))
	for _, msi := range v.GetMediaStreamInfo() {
		if msi.GetType() != "Subtitle" {
			continue
		}

		subtutleType := "srt"
		result, err := url.JoinPath(
			"emby",
			"Videos",
			truePath,
			v.GetId(),
			"Subtitles",
			strconv.FormatUint(msi.GetIndex(), 10),
			"Stream."+subtutleType,
		)
		if err != nil {
			continue
		}
		u.Path = result
		u.RawQuery = ""
		url := u.String()

		name := msi.GetDisplayTitle()
		if name == "" {
			if msi.GetTitle() != "" {
				name = msi.GetTitle()
			} else {
				name = msi.GetDisplayLanguage()
			}
		}

		subtitles = append(subtitles, &EmbySubtitleCache{
			URL:   url,
			Type:  subtutleType,
			Name:  name,
			Cache: refreshcache0.NewRefreshCache(newEmbySubtitleCacheInitFunc(url), -1),
		})
	}
	return subtitles
}

func newEmbySubtitleCacheInitFunc(url string) func(ctx context.Context) ([]byte, error) {
	return func(ctx context.Context) ([]byte, error) {
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
