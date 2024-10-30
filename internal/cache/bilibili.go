package cache

import (
	"bytes"
	"context"
	"encoding/json"
	"errors"
	"fmt"
	"math"
	"net/http"
	"time"

	"github.com/synctv-org/synctv/internal/db"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/bilibili"
	"github.com/zencoder/go-dash/v3/mpd"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/gencontainer/refreshcache0"
	"github.com/zijiren233/gencontainer/refreshcache1"
	"github.com/zijiren233/go-uhc"
)

type BilibiliMpdCache struct {
	Mpd     *mpd.MPD
	HevcMpd *mpd.MPD
	Urls    []string
}

type BilibiliSubtitleCache map[string]*BilibiliSubtitleCacheItem

type BilibiliSubtitleCacheItem struct {
	Srt *refreshcache0.RefreshCache[[]byte]
	Url string
}

func NewBilibiliSharedMpdCacheInitFunc(movie *model.Movie) func(ctx context.Context, args *BilibiliUserCache) (*BilibiliMpdCache, error) {
	return func(ctx context.Context, args *BilibiliUserCache) (*BilibiliMpdCache, error) {
		return BilibiliSharedMpdCacheInitFunc(ctx, movie, args)
	}
}

func BilibiliSharedMpdCacheInitFunc(ctx context.Context, movie *model.Movie, args *BilibiliUserCache) (*BilibiliMpdCache, error) {
	if args == nil {
		return nil, errors.New("no bilibili user cache data")
	}
	var cookies []*http.Cookie
	vendorInfo, err := args.Get(ctx)
	if err != nil {
		if !errors.Is(err, db.ErrNotFound(db.ErrVendorNotFound)) {
			return nil, err
		}
	} else {
		cookies = vendorInfo.Cookies
	}
	cli := vendor.LoadBilibiliClient(movie.MovieBase.VendorInfo.Backend)
	var m, hevcM *mpd.MPD
	biliInfo := movie.MovieBase.VendorInfo.Bilibili
	switch {
	case biliInfo.Epid != 0:
		resp, err := cli.GetDashPGCURL(ctx, &bilibili.GetDashPGCURLReq{
			Cookies: utils.HttpCookieToMap(cookies),
			Epid:    biliInfo.Epid,
		})
		if err != nil {
			return nil, err
		}
		m, err = mpd.ReadFromString(resp.Mpd)
		if err != nil {
			return nil, err
		}
		hevcM, err = mpd.ReadFromString(resp.HevcMpd)
		if err != nil {
			return nil, err
		}

	case biliInfo.Bvid != "":
		resp, err := cli.GetDashVideoURL(ctx, &bilibili.GetDashVideoURLReq{
			Cookies: utils.HttpCookieToMap(cookies),
			Bvid:    biliInfo.Bvid,
			Cid:     biliInfo.Cid,
		})
		if err != nil {
			return nil, err
		}
		m, err = mpd.ReadFromString(resp.Mpd)
		if err != nil {
			return nil, err
		}
		hevcM, err = mpd.ReadFromString(resp.HevcMpd)
		if err != nil {
			return nil, err
		}
	default:
		return nil, errors.New("bvid and epid are empty")
	}
	m.BaseURL = append(m.BaseURL, "/api/room/movie/proxy/")
	id := 0
	movies := []string{}
	for _, p := range m.Periods {
		for _, as := range p.AdaptationSets {
			for _, r := range as.Representations {
				for i := range r.BaseURL {
					movies = append(movies, r.BaseURL[i])
					r.BaseURL[i] = fmt.Sprintf("%s?id=%d&roomId=%s", movie.ID, id, movie.RoomID)
					id++
				}
			}
		}
	}
	for _, p := range hevcM.Periods {
		for _, as := range p.AdaptationSets {
			for _, r := range as.Representations {
				for i := range r.BaseURL {
					movies = append(movies, r.BaseURL[i])
					r.BaseURL[i] = fmt.Sprintf("%s?id=%d&roomId=%s&t=hevc", movie.ID, id, movie.RoomID)
					id++
				}
			}
		}
	}
	return &BilibiliMpdCache{
		Urls:    movies,
		Mpd:     m,
		HevcMpd: hevcM,
	}, nil
}

func BilibiliMpdToString(mpdRaw *mpd.MPD, token string) (string, error) {
	newMpdRaw := *mpdRaw
	newPeriods := make([]*mpd.Period, len(mpdRaw.Periods))
	for i, p := range mpdRaw.Periods {
		n := *p
		newPeriods[i] = &n
	}
	newMpdRaw.Periods = newPeriods
	for _, p := range newMpdRaw.Periods {
		newAdaptationSets := make([]*mpd.AdaptationSet, len(p.AdaptationSets))
		for i, as := range p.AdaptationSets {
			n := *as
			newAdaptationSets[i] = &n
		}
		p.AdaptationSets = newAdaptationSets
		for _, as := range p.AdaptationSets {
			newRepresentations := make([]*mpd.Representation, len(as.Representations))
			for i, r := range as.Representations {
				n := *r
				newRepresentations[i] = &n
			}
			as.Representations = newRepresentations
			for _, r := range as.Representations {
				newBaseURL := make([]string, len(r.BaseURL))
				copy(newBaseURL, r.BaseURL)
				r.BaseURL = newBaseURL
				for i := range r.BaseURL {
					r.BaseURL[i] = fmt.Sprintf("%s&token=%s", r.BaseURL[i], token)
				}
			}
		}
	}
	return newMpdRaw.WriteToString()
}

func NewBilibiliNoSharedMovieCacheInitFunc(movie *model.Movie) func(ctx context.Context, key string, args ...*BilibiliUserCache) (string, error) {
	return func(ctx context.Context, key string, args ...*BilibiliUserCache) (string, error) {
		return BilibiliNoSharedMovieCacheInitFunc(ctx, movie, args...)
	}
}

func BilibiliNoSharedMovieCacheInitFunc(ctx context.Context, movie *model.Movie, args ...*BilibiliUserCache) (string, error) {
	if len(args) == 0 {
		return "", errors.New("no bilibili user cache data")
	}
	var cookies []*http.Cookie
	vendorInfo, err := args[0].Get(ctx)
	if err != nil {
		if !errors.Is(err, db.ErrNotFound(db.ErrVendorNotFound)) {
			return "", err
		}
	} else {
		cookies = vendorInfo.Cookies
	}
	cli := vendor.LoadBilibiliClient(movie.MovieBase.VendorInfo.Backend)
	var u string
	biliInfo := movie.MovieBase.VendorInfo.Bilibili
	switch {
	case biliInfo.Epid != 0:
		resp, err := cli.GetPGCURL(ctx, &bilibili.GetPGCURLReq{
			Cookies: utils.HttpCookieToMap(cookies),
			Epid:    biliInfo.Epid,
		})
		if err != nil {
			return "", err
		}
		u = resp.Url

	case biliInfo.Bvid != "":
		resp, err := cli.GetVideoURL(ctx, &bilibili.GetVideoURLReq{
			Cookies: utils.HttpCookieToMap(cookies),
			Bvid:    biliInfo.Bvid,
			Cid:     biliInfo.Cid,
		})
		if err != nil {
			return "", err
		}
		u = resp.Url

	default:
		return "", errors.New("bvid and epid are empty")

	}

	return u, nil
}

type bilibiliSubtitleResp struct {
	FontColor       string `json:"font_color"`
	BackgroundColor string `json:"background_color"`
	Stroke          string `json:"Stroke"`
	Type            string `json:"type"`
	Lang            string `json:"lang"`
	Version         string `json:"version"`
	Body            []struct {
		Content  string  `json:"content"`
		From     float64 `json:"from"`
		To       float64 `json:"to"`
		Sid      int     `json:"sid"`
		Location int     `json:"location"`
	} `json:"body"`
	FontSize        float64 `json:"font_size"`
	BackgroundAlpha float64 `json:"background_alpha"`
}

func NewBilibiliSubtitleCacheInitFunc(movie *model.Movie) func(ctx context.Context, args *BilibiliUserCache) (BilibiliSubtitleCache, error) {
	return func(ctx context.Context, args *BilibiliUserCache) (BilibiliSubtitleCache, error) {
		return BilibiliSubtitleCacheInitFunc(ctx, movie, args)
	}
}

func BilibiliSubtitleCacheInitFunc(ctx context.Context, movie *model.Movie, args *BilibiliUserCache) (BilibiliSubtitleCache, error) {
	if args == nil {
		return nil, errors.New("no bilibili user cache data")
	}

	biliInfo := movie.MovieBase.VendorInfo.Bilibili
	if biliInfo.Bvid == "" || biliInfo.Cid == 0 {
		return nil, errors.New("bvid or cid is empty")
	}

	// must login
	var cookies []*http.Cookie
	vendorInfo, err := args.Get(ctx)
	if err != nil {
		if errors.Is(err, db.ErrNotFound(db.ErrVendorNotFound)) {
			return nil, nil
		}
	} else {
		cookies = vendorInfo.Cookies
	}

	cli := vendor.LoadBilibiliClient(movie.MovieBase.VendorInfo.Backend)
	resp, err := cli.GetSubtitles(ctx, &bilibili.GetSubtitlesReq{
		Cookies: utils.HttpCookieToMap(cookies),
		Bvid:    biliInfo.Bvid,
		Cid:     biliInfo.Cid,
	})
	if err != nil {
		return nil, err
	}
	subtitleCache := make(BilibiliSubtitleCache, len(resp.Subtitles))
	for k, v := range resp.Subtitles {
		subtitleCache[k] = &BilibiliSubtitleCacheItem{
			Url: v,
			Srt: refreshcache0.NewRefreshCache[[]byte](func(ctx context.Context) ([]byte, error) {
				return translateBilibiliSubtitleToSrt(ctx, v)
			}, 0),
		}
	}

	return subtitleCache, nil
}

func convertToSRT(subtitles *bilibiliSubtitleResp) []byte {
	srt := bytes.NewBuffer(nil)
	counter := 0
	for _, subtitle := range subtitles.Body {
		srt.WriteString(
			fmt.Sprintf("%d\n%s --> %s\n%s\n\n",
				counter,
				formatTime(subtitle.From),
				formatTime(subtitle.To),
				subtitle.Content))
		counter++
	}
	return srt.Bytes()
}

func formatTime(seconds float64) string {
	hours := int(seconds) / 3600
	seconds = math.Mod(seconds, 3600)
	minutes := int(seconds) / 60
	seconds = math.Mod(seconds, 60)
	milliseconds := int((seconds - float64(int(seconds))) * 1000)
	return fmt.Sprintf("%02d:%02d:%02d,%03d", hours, minutes, int(seconds), milliseconds)
}

func translateBilibiliSubtitleToSrt(ctx context.Context, url string) ([]byte, error) {
	r, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https:%s", url), nil)
	if err != nil {
		return nil, err
	}
	r.Header.Set("User-Agent", utils.UA)
	r.Header.Set("Referer", "https://www.bilibili.com")
	resp, err := uhc.Do(r)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	var srt bilibiliSubtitleResp
	err = json.NewDecoder(resp.Body).Decode(&srt)
	if err != nil {
		return nil, err
	}
	return convertToSRT(&srt), nil
}

type BilibiliLiveCache struct{}

func NewBilibiliLiveCacheInitFunc(movie *model.Movie) func(ctx context.Context) ([]byte, error) {
	return func(ctx context.Context) ([]byte, error) {
		return BilibiliLiveCacheInitFunc(ctx, movie)
	}
}

func genBilibiliLiveM3U8ListFile(urls []*bilibili.LiveStream) []byte {
	buf := bytes.NewBuffer(nil)
	buf.WriteString("#EXTM3U\n")
	buf.WriteString("#EXT-X-VERSION:3\n")
	for _, v := range urls {
		if len(v.Urls) == 0 {
			continue
		}
		buf.WriteString(fmt.Sprintf("#EXT-X-STREAM-INF:BANDWIDTH=%d,NAME=\"%s\"\n", 1920*1080*v.Quality, v.Desc))
		buf.WriteString(v.Urls[0] + "\n")
	}
	return buf.Bytes()
}

func BilibiliLiveCacheInitFunc(ctx context.Context, movie *model.Movie) ([]byte, error) {
	cli := vendor.LoadBilibiliClient(movie.MovieBase.VendorInfo.Backend)
	resp, err := cli.GetLiveStreams(ctx, &bilibili.GetLiveStreamsReq{
		Cid: movie.MovieBase.VendorInfo.Bilibili.Cid,
		Hls: true,
	})
	if err != nil {
		return nil, err
	}
	return genBilibiliLiveM3U8ListFile(resp.LiveStreams), nil
}

type BilibiliMovieCache struct {
	NoSharedMovie *MapCache[string, *BilibiliUserCache]
	SharedMpd     *refreshcache1.RefreshCache[*BilibiliMpdCache, *BilibiliUserCache]
	Subtitle      *refreshcache1.RefreshCache[BilibiliSubtitleCache, *BilibiliUserCache]
	Live          *refreshcache0.RefreshCache[[]byte]
}

func NewBilibiliMovieCache(movie *model.Movie) *BilibiliMovieCache {
	return &BilibiliMovieCache{
		NoSharedMovie: newMapCache(NewBilibiliNoSharedMovieCacheInitFunc(movie), time.Minute*60),
		SharedMpd:     refreshcache1.NewRefreshCache(NewBilibiliSharedMpdCacheInitFunc(movie), time.Minute*60),
		Subtitle:      refreshcache1.NewRefreshCache(NewBilibiliSubtitleCacheInitFunc(movie), 0),
		Live:          refreshcache0.NewRefreshCache(NewBilibiliLiveCacheInitFunc(movie), time.Minute*55),
	}
}

type BilibiliUserCache = refreshcache.RefreshCache[*BilibiliUserCacheData, struct{}]

type BilibiliUserCacheData struct {
	Backend string
	Cookies []*http.Cookie
}

func NewBilibiliUserCache(userID string) *BilibiliUserCache {
	f := BilibiliAuthorizationCacheWithUserIDInitFunc(userID)
	return refreshcache.NewRefreshCache(func(ctx context.Context, args ...struct{}) (*BilibiliUserCacheData, error) {
		return f(ctx)
	}, 0)
}

func BilibiliAuthorizationCacheWithUserIDInitFunc(userID string) func(ctx context.Context, args ...struct{}) (*BilibiliUserCacheData, error) {
	return func(ctx context.Context, args ...struct{}) (*BilibiliUserCacheData, error) {
		v, err := db.GetBilibiliVendor(userID)
		if err != nil {
			return nil, err
		}
		return &BilibiliUserCacheData{
			Cookies: utils.MapToHttpCookie(v.Cookies),
			Backend: v.Backend,
		}, nil
	}
}
