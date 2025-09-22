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
	return newMapCache(
		func(ctx context.Context, key string, _ ...struct{}) (*AlistUserCacheData, error) {
			return AlistAuthorizationCacheWithUserIDInitFunc(ctx, userID, key)
		},
		-1,
	)
}

func AlistAuthorizationCacheWithUserIDInitFunc(
	ctx context.Context,
	userID, serverID string,
) (*AlistUserCacheData, error) {
	v, err := db.GetAlistVendor(userID, serverID)
	if err != nil {
		return nil, err
	}

	return AlistAuthorizationCacheWithConfigInitFunc(ctx, v)
}

func AlistAuthorizationCacheWithConfigInitFunc(
	ctx context.Context,
	v *model.AlistVendor,
) (*AlistUserCacheData, error) {
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
		Token:    resp.GetToken(),
		Backend:  v.Backend,
	}, nil
}

type AlistMovieCache = refreshcache1.RefreshCache[*AlistMovieCacheData, *AlistMovieCacheFuncArgs]

func NewAlistMovieCache(movie *model.Movie, subPath string) *AlistMovieCache {
	return refreshcache1.NewRefreshCache(NewAlistMovieCacheInitFunc(movie, subPath), -1)
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
	Ali       *refreshcache0.RefreshCache[*AlistAliCache]
	URL       string
	Provider  string
	Subtitles []*AlistSubtitle
}

type AlistAliCache struct {
	URL          string
	M3U8ListFile []byte
	Subtitles    []*AlistSubtitle
}

type SubtitleDataCache = refreshcache0.RefreshCache[[]byte]

const subtitleMaxLength = 15 * 1024 * 1024

func newAliSubtitles(
	list []*alist.FsOtherResp_VideoPreviewPlayInfo_LiveTranscodingSubtitleTaskList,
) []*AlistSubtitle {
	caches := make([]*AlistSubtitle, len(list))
	for i, v := range list {
		if v.GetStatus() != "finished" {
			return nil
		}

		url := v.GetUrl()
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

				if resp.ContentLength > subtitleMaxLength {
					return nil, fmt.Errorf(
						"subtitle too large, got: %d, max: %d",
						resp.ContentLength,
						subtitleMaxLength,
					)
				}

				return io.ReadAll(io.LimitReader(resp.Body, subtitleMaxLength))
			}, -1),
			Name: v.GetLanguage(),
			URL:  v.GetUrl(),
			Type: utils.GetFileExtension(v.GetUrl()),
		}
	}

	return caches
}

func genAliM3U8ListFile(
	urls []*alist.FsOtherResp_VideoPreviewPlayInfo_LiveTranscodingTaskList,
) []byte {
	buf := bytes.NewBuffer(nil)
	buf.WriteString("#EXTM3U\n")
	buf.WriteString("#EXT-X-VERSION:3\n")

	for _, v := range urls {
		if v.GetStatus() != "finished" {
			return nil
		}

		fmt.Fprintf(
			buf,
			"#EXT-X-STREAM-INF:BANDWIDTH=%d,RESOLUTION=%dx%d,NAME=\"%d\"\n",
			v.GetTemplateWidth()*v.GetTemplateHeight(),
			v.GetTemplateWidth(),
			v.GetTemplateHeight(),
			v.GetTemplateWidth(),
		)
		buf.WriteString(v.GetUrl() + "\n")
	}

	return buf.Bytes()
}

type AlistMovieCacheFuncArgs struct {
	UserCache *AlistUserCache
	UserAgent string
}

func NewAlistMovieCacheInitFunc(
	movie *model.Movie,
	subPath string,
) func(ctx context.Context, args *AlistMovieCacheFuncArgs) (*AlistMovieCacheData, error) {
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

		cli := vendor.LoadAlistClient(movie.VendorInfo.Backend)

		fg, err := getFsGet(
			ctx,
			cli,
			aucd,
			truePath,
			movie.VendorInfo.Alist.Password,
			args.UserAgent,
		)
		if err != nil {
			return nil, err
		}

		if fg.GetIsDir() {
			return nil, fmt.Errorf("path is dir: %s", truePath)
		}

		cache := &AlistMovieCacheData{
			URL:      fg.GetRawUrl(),
			Provider: fg.GetProvider(),
		}

		if err := processSubtitles(ctx, cli, aucd, fg, truePath, movie.VendorInfo.Alist.Password, args.UserAgent, cache); err != nil {
			return nil, err
		}

		if fg.GetProvider() == AlistProviderAli {
			processAliProvider(
				ctx,
				fg.GetRawUrl(),
				cli,
				aucd,
				truePath,
				movie.VendorInfo.Alist.Password,
				cache,
			)
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
	serverID, truePath, err := movie.VendorInfo.Alist.ServerIDAndFilePath()
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

func getFsGet(
	ctx context.Context,
	cli alist.AlistHTTPServer,
	aucd *AlistUserCacheData,
	truePath, password, userAgent string,
) (*alist.FsGetResp, error) {
	return cli.FsGet(ctx, &alist.FsGetReq{
		Host:      aucd.Host,
		Token:     aucd.Token,
		Path:      truePath,
		Password:  password,
		UserAgent: userAgent,
	})
}

func processSubtitles(
	ctx context.Context,
	cli alist.AlistHTTPServer,
	aucd *AlistUserCacheData,
	fg *alist.FsGetResp,
	truePath, password, userAgent string,
	cache *AlistMovieCacheData,
) error {
	prefix := strings.TrimSuffix(truePath, fg.GetName())
	for _, related := range fg.GetRelated() {
		if related.GetType() != 4 {
			continue
		}

		if utils.GetFileExtension(related.GetName()) == "xml" {
			continue
		}

		resp, err := getFsGet(ctx, cli, aucd, prefix+related.GetName(), password, userAgent)
		if err != nil {
			return err
		}

		subtitle := &AlistSubtitle{
			Name: related.GetName(),
			URL:  resp.GetRawUrl(),
			Type: utils.GetFileExtension(resp.GetName()),
			Cache: refreshcache0.NewRefreshCache(func(ctx context.Context) ([]byte, error) {
				return fetchSubtitleContent(ctx, resp.GetRawUrl())
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

	if resp.ContentLength > subtitleMaxLength {
		return nil, fmt.Errorf(
			"subtitle too large, got: %d, max: %d",
			resp.ContentLength,
			subtitleMaxLength,
		)
	}

	return io.ReadAll(io.LimitReader(resp.Body, subtitleMaxLength))
}

func processAliProvider(
	_ context.Context,
	firstURL string,
	cli alist.AlistHTTPServer,
	aucd *AlistUserCacheData,
	truePath, password string,
	cache *AlistMovieCacheData,
) {
	cache.Ali = refreshcache0.NewRefreshCache(func(ctx context.Context) (*AlistAliCache, error) {
		var url string
		if firstURL != "" {
			url = firstURL
			firstURL = ""
		} else {
			u, err := cli.FsGet(ctx, &alist.FsGetReq{
				Host:     aucd.Host,
				Token:    aucd.Token,
				Path:     truePath,
				Password: password,
			})
			if err != nil {
				return nil, err
			}

			url = u.GetRawUrl()
		}

		fo, err := cli.FsOther(ctx, &alist.FsOtherReq{
			Host:     aucd.Host,
			Token:    aucd.Token,
			Path:     truePath,
			Password: password,
			Method:   "video_preview",
		})
		if err != nil {
			return nil, err
		}

		return &AlistAliCache{
			URL: url,
			M3U8ListFile: genAliM3U8ListFile(
				fo.GetVideoPreviewPlayInfo().GetLiveTranscodingTaskList(),
			),
			Subtitles: newAliSubtitles(
				fo.GetVideoPreviewPlayInfo().GetLiveTranscodingSubtitleTaskList(),
			),
		}, nil
	}, 14*time.Minute)
}
