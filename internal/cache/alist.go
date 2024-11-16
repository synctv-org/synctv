package cache

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"path"
	"strings"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
	"github.com/zijiren233/gencontainer/refreshcache0"
	"github.com/zijiren233/gencontainer/refreshcache1"
	"github.com/zijiren233/go-uhc"
)

type AlistUserCache = MapCache[*AlistUserCacheData, struct{}]

type AlistUserCacheData struct {
	Host     string
	ServerID string
	Token    string
	Backend  string
}

func NewAlistUserCache(userID string) *AlistUserCache {
	return newMapCache[*AlistUserCacheData, struct{}](func(ctx context.Context, key string, args ...struct{}) (*AlistUserCacheData, error) {
		return AlistAuthorizationCacheWithUserIDInitFunc(ctx, userID, key)
	}, -1)
}

func AlistAuthorizationCacheWithUserIDInitFunc(ctx context.Context, userID, serverID string) (*AlistUserCacheData, error) {
	v, err := db.GetAlistVendor(userID, serverID)
	if err != nil {
		return nil, err
	}
	return AlistAuthorizationCacheWithConfigInitFunc(ctx, v)
}

func AlistAuthorizationCacheWithConfigInitFunc(ctx context.Context, v *model.AlistVendor) (*AlistUserCacheData, error) {
	cli := vendor.LoadAlistClient(v.Backend)
	model.GenAlistServerID(v)

	if v.Username == "" {
		_, err := cli.Me(ctx, &alist.MeReq{
			Host: v.Host,
		})
		if err != nil {
			return nil, err
		}
		return &AlistUserCacheData{
			Host:     v.Host,
			ServerID: v.ServerID,
			Backend:  v.Backend,
		}, nil
	}
	resp, err := cli.Login(ctx, &alist.LoginReq{
		Host:     v.Host,
		Username: v.Username,
		Password: string(v.HashedPassword),
		Hashed:   true,
	})
	if err != nil {
		return nil, err
	}

	return &AlistUserCacheData{
		Host:     v.Host,
		ServerID: v.ServerID,
		Token:    resp.Token,
		Backend:  v.Backend,
	}, nil
}

type AlistMovieCache = refreshcache1.RefreshCache[*AlistMovieCacheData, *AlistMovieCacheFuncArgs]

func NewAlistMovieCache(movie *model.Movie, subPath string) *AlistMovieCache {
	return refreshcache1.NewRefreshCache(NewAlistMovieCacheInitFunc(movie, subPath), time.Minute*14)
}

type AlistProvider = string

const (
	AlistProviderAli = "AliyundriveOpen"
	AlistProvider115 = "115 Cloud"
)

type AlistSubtitle struct {
	Cache *SubtitleDataCache
	Name  string
	URL   string
	Type  string
}

type AlistMovieCacheData struct {
	Ali       *AlistAliCache
	URL       string
	Provider  string
	Subtitles []*AlistSubtitle
}

type AlistAliCache struct {
	M3U8ListFile []byte
}

type SubtitleDataCache = refreshcache0.RefreshCache[[]byte]

func newAliSubtitles(list []*alist.FsOtherResp_VideoPreviewPlayInfo_LiveTranscodingSubtitleTaskList) []*AlistSubtitle {
	caches := make([]*AlistSubtitle, len(list))
	for i, v := range list {
		if v.Status != "finished" {
			return nil
		}
		url := v.Url
		caches[i] = &AlistSubtitle{
			Cache: refreshcache0.NewRefreshCache(func(ctx context.Context) ([]byte, error) {
				r, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
				if err != nil {
					return nil, err
				}
				resp, err := uhc.Do(r)
				if err != nil {
					return nil, err
				}
				defer resp.Body.Close()
				if resp.StatusCode != http.StatusOK {
					return nil, fmt.Errorf("status code: %d", resp.StatusCode)
				}
				return io.ReadAll(resp.Body)
			}, -1),
			Name: v.Language,
			URL:  v.Url,
			Type: utils.GetFileExtension(v.Url),
		}
	}
	return caches
}

func genAliM3U8ListFile(urls []*alist.FsOtherResp_VideoPreviewPlayInfo_LiveTranscodingTaskList) []byte {
	buf := bytes.NewBuffer(nil)
	buf.WriteString("#EXTM3U\n")
	buf.WriteString("#EXT-X-VERSION:3\n")
	for _, v := range urls {
		if v.Status != "finished" {
			return nil
		}
		buf.WriteString(fmt.Sprintf("#EXT-X-STREAM-INF:BANDWIDTH=%d,RESOLUTION=%dx%d,NAME=\"%d\"\n", v.TemplateWidth*v.TemplateHeight, v.TemplateWidth, v.TemplateHeight, v.TemplateWidth))
		buf.WriteString(v.Url + "\n")
	}
	return buf.Bytes()
}

type AlistMovieCacheFuncArgs struct {
	UserCache *AlistUserCache
	UserAgent string
}

func NewAlistMovieCacheInitFunc(movie *model.Movie, subPath string) func(ctx context.Context, args *AlistMovieCacheFuncArgs) (*AlistMovieCacheData, error) {
	return func(ctx context.Context, args *AlistMovieCacheFuncArgs) (*AlistMovieCacheData, error) {
		if err := validateArgs(args, movie, subPath); err != nil {
			return nil, err
		}

		serverID, truePath, err := getServerIDAndPath(movie, subPath)
		if err != nil {
			return nil, err
		}

		aucd, err := args.UserCache.LoadOrStore(ctx, serverID)
		if err != nil {
			return nil, err
		}
		if aucd.Host == "" {
			return nil, errors.New("not bind alist vendor")
		}

		cli := vendor.LoadAlistClient(movie.MovieBase.VendorInfo.Backend)
		fg, err := getFsGet(ctx, cli, aucd, truePath, movie.MovieBase.VendorInfo.Alist.Password, args.UserAgent)
		if err != nil {
			return nil, err
		}

		if fg.IsDir {
			return nil, fmt.Errorf("path is dir: %s", truePath)
		}

		cache := &AlistMovieCacheData{
			URL:      fg.RawUrl,
			Provider: fg.Provider,
		}

		if err := processSubtitles(ctx, cli, aucd, fg, truePath, movie.MovieBase.VendorInfo.Alist.Password, args.UserAgent, cache); err != nil {
			return nil, err
		}

		if fg.Provider == AlistProviderAli {
			if err := processAliProvider(ctx, cli, aucd, truePath, movie.MovieBase.VendorInfo.Alist.Password, cache); err != nil {
				return nil, err
			}
		}

		return cache, nil
	}
}

func validateArgs(args *AlistMovieCacheFuncArgs, movie *model.Movie, subPath string) error {
	if args == nil {
		return errors.New("need alist user cache")
	}
	if args.UserCache == nil {
		return errors.New("need alist user cache")
	}
	if movie.IsFolder && subPath == "" {
		return errors.New("sub path is empty")
	}
	return nil
}

func getServerIDAndPath(movie *model.Movie, subPath string) (string, string, error) {
	serverID, truePath, err := movie.MovieBase.VendorInfo.Alist.ServerIDAndFilePath()
	if err != nil {
		return "", "", err
	}

	if movie.IsFolder {
		newPath := path.Join(truePath, subPath)
		if !strings.HasPrefix(newPath, truePath) {
			return "", "", errors.New("sub path is not in parent path")
		}
		truePath = newPath
	}

	return serverID, truePath, nil
}

func getFsGet(ctx context.Context, cli alist.AlistHTTPServer, aucd *AlistUserCacheData, truePath, password, userAgent string) (*alist.FsGetResp, error) {
	return cli.FsGet(ctx, &alist.FsGetReq{
		Host:      aucd.Host,
		Token:     aucd.Token,
		Path:      truePath,
		Password:  password,
		UserAgent: userAgent,
	})
}

func processSubtitles(ctx context.Context, cli alist.AlistHTTPServer, aucd *AlistUserCacheData, fg *alist.FsGetResp, truePath, password, userAgent string, cache *AlistMovieCacheData) error {
	prefix := strings.TrimSuffix(truePath, fg.Name)
	for _, related := range fg.Related {
		if related.Type != 4 {
			continue
		}
		if utils.GetFileExtension(related.Name) == "xml" {
			continue
		}

		resp, err := getFsGet(ctx, cli, aucd, prefix+related.Name, password, userAgent)
		if err != nil {
			return err
		}

		subtitle := &AlistSubtitle{
			Name: related.Name,
			URL:  resp.RawUrl,
			Type: utils.GetFileExtension(resp.Name),
			Cache: refreshcache0.NewRefreshCache(func(ctx context.Context) ([]byte, error) {
				return fetchSubtitleContent(ctx, resp.RawUrl)
			}, -1),
		}
		cache.Subtitles = append(cache.Subtitles, subtitle)
	}
	return nil
}

func fetchSubtitleContent(ctx context.Context, url string) ([]byte, error) {
	r, err := http.NewRequestWithContext(ctx, http.MethodGet, url, nil)
	if err != nil {
		return nil, err
	}
	resp, err := uhc.Do(r)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	if resp.StatusCode != http.StatusOK {
		return nil, fmt.Errorf("status code: %d", resp.StatusCode)
	}
	return io.ReadAll(resp.Body)
}

func processAliProvider(ctx context.Context, cli alist.AlistHTTPServer, aucd *AlistUserCacheData, truePath, password string, cache *AlistMovieCacheData) error {
	fo, err := cli.FsOther(ctx, &alist.FsOtherReq{
		Host:     aucd.Host,
		Token:    aucd.Token,
		Path:     truePath,
		Password: password,
		Method:   "video_preview",
	})
	if err != nil {
		return err
	}

	cache.Ali = &AlistAliCache{
		M3U8ListFile: genAliM3U8ListFile(fo.VideoPreviewPlayInfo.LiveTranscodingTaskList),
	}
	cache.Subtitles = append(cache.Subtitles, newAliSubtitles(fo.VideoPreviewPlayInfo.LiveTranscodingSubtitleTaskList)...)
	return nil
}
