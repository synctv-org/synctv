package handlers

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"io"
	"math"
	"math/rand"
	"net/http"
	"net/url"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/internal/vendor"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
	"github.com/synctv-org/vendors/api/bilibili"
	"github.com/zencoder/go-dash/v3/mpd"
	"github.com/zijiren233/gencontainer/refreshcache"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
	"golang.org/x/exp/maps"
)

func GetPageAndPageSize(ctx *gin.Context) (int, int, error) {
	pageSize, err := strconv.Atoi(ctx.DefaultQuery("max", "10"))
	if err != nil {
		return 0, 0, errors.New("max must be a number")
	}
	page, err := strconv.Atoi(ctx.DefaultQuery("page", "1"))
	if err != nil {
		return 0, 0, errors.New("page must be a number")
	}
	return page, pageSize, nil
}

func GetPageItems[T any](ctx *gin.Context, items []T) ([]T, error) {
	page, max, err := GetPageAndPageSize(ctx)
	if err != nil {
		return nil, err
	}

	return utils.GetPageItems(items, page, max), nil
}

func MovieList(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	page, max, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m := room.GetMoviesWithPage(page, max)

	current, err := genCurrent(ctx, room.Current(), user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	mresp := make([]model.MoviesResp, len(m))
	for i, v := range m {
		mresp[i] = model.MoviesResp{
			Id:      v.Movie.ID,
			Base:    v.Movie.Base,
			Creator: op.GetUserName(v.Movie.CreatorID),
		}
		// hide url and headers when proxy
		if user.ID != v.Movie.CreatorID && v.Movie.Base.Proxy {
			mresp[i].Base.Url = ""
			mresp[i].Base.Headers = nil
		}
	}

	current.UpdateSeek()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": genCurrentResp(current),
		"total":   room.GetMoviesCount(),
		"movies":  mresp,
	}))
}

func genCurrent(ctx context.Context, current *op.Current, userID string) (*op.Current, error) {
	if current.Movie.Movie.Base.VendorInfo.Vendor != "" {
		return current, parse2VendorMovie(ctx, userID, &current.Movie)
	}
	return current, nil
}

func genCurrentResp(current *op.Current) *model.CurrentMovieResp {
	c := &model.CurrentMovieResp{
		Status: current.Status,
		Movie: model.MoviesResp{
			Id:      current.Movie.Movie.ID,
			Base:    current.Movie.Movie.Base,
			Creator: op.GetUserName(current.Movie.Movie.CreatorID),
		},
	}
	if c.Movie.Base.Type == "" && c.Movie.Base.Url != "" {
		c.Movie.Base.Type = utils.GetUrlExtension(c.Movie.Base.Url)
	}
	// hide url and headers when proxy
	if c.Movie.Base.Proxy {
		c.Movie.Base.Url = ""
		c.Movie.Base.Headers = nil
	}
	return c
}

func CurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	current, err := genCurrent(ctx, room.Current(), user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	current.UpdateSeek()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(genCurrentResp(current)))
}

func Movies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	page, max, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m := room.GetMoviesWithPage(int(page), int(max))

	mresp := make([]*model.MoviesResp, len(m))
	for i, v := range m {
		mresp[i] = &model.MoviesResp{
			Id:      v.Movie.ID,
			Base:    v.Movie.Base,
			Creator: op.GetUserName(v.Movie.CreatorID),
		}
		// hide url and headers when proxy
		if user.ID != v.Movie.CreatorID && v.Movie.Base.Proxy {
			mresp[i].Base.Url = ""
			mresp[i].Base.Headers = nil
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"total":  room.GetMoviesCount(),
		"movies": mresp,
	}))
}

func PushMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.PushMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.AddMovieToRoom(room, (*dbModel.BaseMovie)(&req))
	if err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func PushMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.PushMoviesReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var ms []*dbModel.BaseMovie = make([]*dbModel.BaseMovie, len(req))

	for i, v := range req {
		m := (*dbModel.BaseMovie)(v)
		ms[i] = m
	}

	err := user.AddMoviesToRoom(room, ms)
	if err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func NewPublishKey(ctx *gin.Context) {
	if !conf.Conf.Server.Rtmp.Enable {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	}

	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	movie, err := room.GetMovieByID(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if movie.Movie.CreatorID != user.ID && !user.HasRoomPermission(room, dbModel.PermissionEditUser) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(dbModel.ErrNoPermission))
		return
	}

	if !movie.Movie.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("only live movie can get publish key"))
		return
	}

	token, err := rtmp.NewRtmpAuthorization(movie.Movie.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	host := settings.CustomPublishHost.Get()
	if host == "" {
		host = ctx.Request.Host
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"host":  host,
		"app":   room.ID,
		"token": token,
	}))
}

func EditMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.EditMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.UpdateMovie(room, req.Id, (*dbModel.BaseMovie)(&req.PushMovieReq)); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DelMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.IdsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.DeleteMoviesByID(room, req.Ids)
	if err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ClearMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	if err := user.ClearMovies(room); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SwapMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.SwapMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.SwapMoviePositions(req.Id1, req.Id2); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_MOVIES,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ChangeCurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.IdCanEmptyReq{}
	err := model.Decode(ctx, &req)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if req.Id == "" {
		err = user.SetCurrentMovie(room, nil, false)
	} else {
		err = user.SetCurrentMovieByID(room, req.Id, true)
	}
	if err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
	}

	if err := room.Broadcast(&op.ElementMessage{
		Type:   pb.ElementMessageType_CHANGE_CURRENT,
		Sender: user.Username,
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ProxyMovie(ctx *gin.Context) {
	if !settings.MovieProxy.Get() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("movie proxy is not enabled"))
		return
	}
	roomId := ctx.Param("roomId")
	if roomId == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("roomId is empty"))
		return
	}

	room, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m, err := room.GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !m.Movie.Base.Proxy || m.Movie.Base.Live || m.Movie.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
		return
	}

	if m.Movie.Base.VendorInfo.Vendor != "" {
		proxyVendorMovie(ctx, m)
		return
	}

	switch m.Movie.Base.Type {
	case "mpd":
		mpdI, err := m.Cache().LoadOrStore("", initDashCache(ctx, &m.Movie), time.Minute*5)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		mpd, ok := mpdI.(string)
		if !ok {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("cache type error"))
			return
		}
		ctx.Data(http.StatusOK, "application/dash+xml", []byte(mpd))
		return
	default:
		err = proxyURL(ctx, m.Movie.Base.Url, m.Movie.Base.Headers)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}
}

// only cache mpd file
func initDashCache(ctx context.Context, movie *dbModel.Movie) func() (any, error) {
	return func() (any, error) {
		req, err := http.NewRequestWithContext(ctx, http.MethodGet, movie.Base.Url, nil)
		if err != nil {
			return nil, err
		}
		for k, v := range movie.Base.Headers {
			req.Header.Set(k, v)
		}
		req.Header.Set("User-Agent", utils.UA)
		resp, err := http.DefaultClient.Do(req)
		if err != nil {
			return nil, err
		}
		defer resp.Body.Close()
		b, err := io.ReadAll(resp.Body)
		if err != nil {
			return nil, err
		}
		m, err := mpd.ReadFromString(string(b))
		if err != nil {
			return nil, err
		}
		if len(m.BaseURL) != 0 && !path.IsAbs(m.BaseURL[0]) {
			result, err := url.JoinPath(path.Dir(movie.Base.Url), m.BaseURL[0])
			if err != nil {
				return nil, err
			}
			m.BaseURL = []string{result}
		}
		s, err := m.WriteToString()
		if err != nil {
			return nil, err
		}
		return s, nil
	}
}

func proxyURL(ctx *gin.Context, u string, headers map[string]string) error {
	if !settings.AllowProxyToLocal.Get() {
		if l, err := utils.ParseURLIsLocalIP(u); err != nil {
			return err
		} else if l {
			return errors.New("not allow proxy to local")
		}
	}
	ctx2, cf := context.WithCancel(ctx)
	defer cf()
	req, err := http.NewRequestWithContext(ctx2, http.MethodGet, u, nil)
	if err != nil {
		return err
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	req.Header.Set("Range", ctx.GetHeader("Range"))
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	ctx.Header("Content-Type", resp.Header.Get("Content-Type"))
	ctx.Header("Content-Length", resp.Header.Get("Content-Length"))
	ctx.Header("Accept-Ranges", resp.Header.Get("Accept-Ranges"))
	ctx.Header("Cache-Control", resp.Header.Get("Cache-Control"))
	ctx.Header("Content-Range", resp.Header.Get("Content-Range"))
	ctx.Status(resp.StatusCode)
	io.Copy(ctx.Writer, resp.Body)
	return nil
}

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinLive(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	// user := ctx.MustGet("user").(*op.User)

	movieId := strings.Trim(ctx.Param("movieId"), "/")
	fileExt := path.Ext(movieId)
	splitedMovieId := strings.Split(movieId, "/")
	channelName := strings.TrimSuffix(splitedMovieId[0], fileExt)
	m, err := room.GetMovieByID(channelName)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if m.Movie.Base.RtmpSource && !conf.Conf.Server.Rtmp.Enable {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	} else if m.Movie.Base.Live && !settings.LiveProxy.Get() {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("live proxy is not enabled"))
		return
	}
	channel, err := m.Channel()
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if channel == nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorStringResp("channel is nil"))
		return
	}

	switch fileExt {
	case ".flv":
		ctx.Header("Cache-Control", "no-store")
		w := httpflv.NewHttpFLVWriter(ctx.Writer)
		defer w.Close()
		channel.AddPlayer(w)
		w.SendPacket()
	case ".m3u8":
		ctx.Header("Cache-Control", "no-store")
		b, err := channel.GenM3U8File(func(tsName string) (tsPath string) {
			ext := "ts"
			if settings.TsDisguisedAsPng.Get() {
				ext = "png"
			}
			return fmt.Sprintf("/api/movie/live/%s/%s.%s", channelName, tsName, ext)
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
	case ".ts":
		if settings.TsDisguisedAsPng.Get() {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(splitedMovieId[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Data(http.StatusOK, hls.TSContentType, b)
	case ".png":
		if !settings.TsDisguisedAsPng.Get() {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(splitedMovieId[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		img := image.NewGray(image.Rect(0, 0, 1, 1))
		img.Set(1, 1, color.Gray{uint8(rand.Intn(255))})
		cache := bytes.NewBuffer(make([]byte, 0, 71))
		err = png.Encode(cache, img)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, "image/png", append(cache.Bytes(), b...))
	default:
		ctx.Header("Cache-Control", "no-store")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
	}
}

type bilibiliCache struct {
	mpd     string
	hevcMpd string
	urls    []string
}

func initBilibiliMPDCache(ctx context.Context, movie dbModel.Movie) func() (any, error) {
	return func() (any, error) {
		var cookies []*http.Cookie
		vendorInfo, err := db.GetVendorByUserIDAndVendor(movie.CreatorID, dbModel.StreamingVendorBilibili)
		if err != nil {
			if !errors.Is(err, db.ErrNotFound("vendor")) {
				return nil, err
			}
		} else {
			cookies = vendorInfo.Cookies
		}
		cli := vendor.BilibiliClient(movie.Base.VendorInfo.Backend)
		var m, hevcM *mpd.MPD
		biliInfo := movie.Base.VendorInfo.Bilibili
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
		m.BaseURL = append(m.BaseURL, fmt.Sprintf("/api/movie/proxy/%s/", movie.RoomID))
		id := 0
		movies := []string{}
		for _, p := range m.Periods {
			for _, as := range p.AdaptationSets {
				for _, r := range as.Representations {
					for i := range r.BaseURL {
						movies = append(movies, r.BaseURL[i])
						r.BaseURL[i] = fmt.Sprintf("%s?id=%d", movie.ID, id)
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
						r.BaseURL[i] = fmt.Sprintf("%s?id=%d&t=hevc", movie.ID, id)
						id++
					}
				}
			}
		}
		s, err := m.WriteToString()
		if err != nil {
			return nil, err
		}
		s2, err := hevcM.WriteToString()
		if err != nil {
			return nil, err
		}
		return &bilibiliCache{
			urls:    movies,
			mpd:     s,
			hevcMpd: s2,
		}, nil
	}
}

func initBilibiliCache(ctx context.Context, movie dbModel.Movie, cookieUserID string) func() (any, error) {
	return func() (any, error) {
		var cookies []*http.Cookie
		vendorInfo, err := db.GetVendorByUserIDAndVendor(cookieUserID, dbModel.StreamingVendorBilibili)
		if err != nil {
			if !errors.Is(err, db.ErrNotFound("vendor")) {
				return nil, err
			}
		} else {
			cookies = vendorInfo.Cookies
		}
		cli := vendor.BilibiliClient(movie.Base.VendorInfo.Backend)
		var u string
		biliInfo := movie.Base.VendorInfo.Bilibili
		switch {
		case biliInfo.Epid != 0:
			resp, err := cli.GetPGCURL(ctx, &bilibili.GetPGCURLReq{
				Cookies: utils.HttpCookieToMap(cookies),
				Epid:    biliInfo.Epid,
			})
			if err != nil {
				return nil, err
			}
			u = resp.Url

		case biliInfo.Bvid != "":
			resp, err := cli.GetVideoURL(ctx, &bilibili.GetVideoURLReq{
				Cookies: utils.HttpCookieToMap(cookies),
				Bvid:    biliInfo.Bvid,
				Cid:     biliInfo.Cid,
			})
			if err != nil {
				return nil, err
			}
			u = resp.Url

		default:
			return nil, errors.New("bvid and epid are empty")

		}

		return u, nil
	}
}

type bilibiliSubtitleCache map[string]*struct {
	url string
	srt *refreshcache.RefreshData[[]byte]
}

type bilibiliSubtitleResp struct {
	FontSize        float64 `json:"font_size"`
	FontColor       string  `json:"font_color"`
	BackgroundAlpha float64 `json:"background_alpha"`
	BackgroundColor string  `json:"background_color"`
	Stroke          string  `json:"Stroke"`
	Type            string  `json:"type"`
	Lang            string  `json:"lang"`
	Version         string  `json:"version"`
	Body            []struct {
		From     float64 `json:"from"`
		To       float64 `json:"to"`
		Sid      int     `json:"sid"`
		Location int     `json:"location"`
		Content  string  `json:"content"`
	} `json:"body"`
}

func initBilibiliSubtitleCache(ctx context.Context, movie dbModel.Movie) func() (any, error) {
	return func() (any, error) {
		biliInfo := movie.Base.VendorInfo.Bilibili
		if biliInfo.Bvid == "" || biliInfo.Cid == 0 {
			return nil, errors.New("bvid or cid is empty")
		}

		var cookies []*http.Cookie
		vendorInfo, err := db.GetVendorByUserIDAndVendor(movie.CreatorID, dbModel.StreamingVendorBilibili)
		if err != nil {
			if !errors.Is(err, db.ErrNotFound("vendor")) {
				return nil, err
			}
		} else {
			cookies = vendorInfo.Cookies
		}
		cli := vendor.BilibiliClient(movie.Base.VendorInfo.Backend)
		resp, err := cli.GetSubtitles(ctx, &bilibili.GetSubtitlesReq{
			Cookies: utils.HttpCookieToMap(cookies),
			Bvid:    biliInfo.Bvid,
			Cid:     biliInfo.Cid,
		})
		if err != nil {
			return nil, err
		}
		subtitleCache := make(bilibiliSubtitleCache, len(resp.Subtitles))
		for k, v := range resp.Subtitles {
			subtitleCache[k] = &struct {
				url string
				srt *refreshcache.RefreshData[[]byte]
			}{
				url: v,
				srt: refreshcache.NewRefreshData[[]byte](0),
			}
		}

		return subtitleCache, nil
	}
}

func convertToSRT(subtitles bilibiliSubtitleResp) []byte {
	srt := bytes.NewBuffer(nil)
	counter := 1
	for _, subtitle := range subtitles.Body {
		start := formatTime(subtitle.From)
		end := formatTime(subtitle.To)
		srt.WriteString(fmt.Sprintf("%d\n%s --> %s\n%s\n\n", counter, start, end, subtitle.Content))
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
	resp, err := http.DefaultClient.Do(r)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	var srt bilibiliSubtitleResp
	err = json.NewDecoder(resp.Body).Decode(&srt)
	if err != nil {
		return nil, err
	}
	return convertToSRT(srt), nil
}

type alistCache struct {
	url string
}

func initAlistCache(ctx context.Context, movie dbModel.Movie) func() (any, error) {
	return func() (any, error) {
		v, err := db.GetVendorByUserIDAndVendor(movie.CreatorID, dbModel.StreamingVendorAlist)
		if err != nil {
			return nil, err
		}
		if v.Host == "" {
			return nil, errors.New("not bind alist vendor")
		}
		cli := vendor.AlistClient(movie.Base.VendorInfo.Backend)
		fg, err := cli.FsGet(ctx, &alist.FsGetReq{
			Host:     v.Host,
			Token:    v.Authorization,
			Path:     movie.Base.VendorInfo.Alist.Path,
			Password: movie.Base.VendorInfo.Alist.Password,
		})
		if err != nil {
			return nil, err
		}

		if fg.IsDir {
			return nil, errors.New("path is dir")
		}

		cache := &alistCache{
			url: fg.RawUrl,
		}
		if fg.Provider == "AliyundriveOpen" {
			fo, err := cli.FsOther(ctx, &alist.FsOtherReq{
				Host:     v.Host,
				Token:    v.Authorization,
				Path:     movie.Base.VendorInfo.Alist.Path,
				Password: movie.Base.VendorInfo.Alist.Password,
				Method:   "video_preview",
			})
			if err != nil {
				return nil, err
			}
			cache.url = fo.VideoPreviewPlayInfo.LiveTranscodingTaskList[len(fo.VideoPreviewPlayInfo.LiveTranscodingTaskList)-1].Url
		}
		return cache, nil
	}
}

func proxyVendorMovie(ctx *gin.Context, movie *op.Movie) {
	switch movie.Movie.Base.VendorInfo.Vendor {
	case dbModel.StreamingVendorBilibili:
		t := ctx.Query("t")
		switch t {
		case "", "hevc":
			mpdI, err := movie.Cache().LoadOrStore(t, initBilibiliMPDCache(ctx, movie.Movie), time.Minute*119)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			mpd, ok := mpdI.(*bilibiliCache)
			if !ok {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("cache type error"))
				return
			}
			if id := ctx.Query("id"); id == "" {
				if t == "hevc" {
					ctx.Data(http.StatusOK, "application/dash+xml", []byte(mpd.hevcMpd))
				} else {
					ctx.Data(http.StatusOK, "application/dash+xml", []byte(mpd.mpd))
				}
				return
			} else {
				streamId, err := strconv.Atoi(id)
				if err != nil {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
					return
				}
				if streamId >= len(mpd.urls) {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("stream id out of range"))
					return
				}
				headers := maps.Clone(movie.Movie.Base.Headers)
				if headers == nil {
					headers = map[string]string{
						"Referer":    "https://www.bilibili.com",
						"User-Agent": utils.UA,
					}
				} else {
					headers["Referer"] = "https://www.bilibili.com"
					headers["User-Agent"] = utils.UA
				}
				proxyURL(ctx, mpd.urls[streamId], headers)
				return
			}
		case "subtitle":
			id := ctx.Query("n")
			if id == "" {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("n is empty"))
				return
			}
			srtI, err := movie.Cache().LoadOrStore("subtitle", initBilibiliSubtitleCache(ctx, movie.Movie), time.Minute*15)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			srtFunc, ok := srtI.(bilibiliSubtitleCache)
			if !ok {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("subtitle cache type error"))
				return
			}
			if s, ok := srtFunc[id]; ok {
				srtData, err := s.srt.Get(func() ([]byte, error) {
					return translateBilibiliSubtitleToSrt(ctx, s.url)
				})
				if err != nil {
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				ctx.Data(http.StatusOK, "text/plain; charset=utf-8", srtData)
				return
			} else {
				ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorStringResp("subtitle not found"))
				return
			}
		}

	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("vendor not support"))
		return
	}
}

func parse2VendorMovie(ctx context.Context, userID string, movie *op.Movie) (err error) {
	if movie.Movie.Base.VendorInfo.Shared {
		userID = movie.Movie.CreatorID
	}

	switch movie.Movie.Base.VendorInfo.Vendor {
	case dbModel.StreamingVendorBilibili:
		if !movie.Movie.Base.Proxy {
			dataI, err := movie.Cache().LoadOrStore(userID, initBilibiliCache(ctx, movie.Movie, userID), time.Minute*119)
			if err != nil {
				return err
			}

			data, ok := dataI.(string)
			if !ok {
				return errors.New("cache type error")
			}

			movie.Movie.Base.Url = data
		} else {
			movie.Movie.Base.Type = "mpd"
		}
		srtI, err := movie.Cache().LoadOrStore("subtitle", initBilibiliSubtitleCache(ctx, movie.Movie), time.Minute*15)
		if err != nil {
			return err
		}
		srt, ok := srtI.(bilibiliSubtitleCache)
		if !ok {
			return errors.New("subtitle cache type error")
		}
		for k := range srt {
			if movie.Movie.Base.Subtitles == nil {
				movie.Movie.Base.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
			}
			movie.Movie.Base.Subtitles[k] = &dbModel.Subtitle{
				URL:  fmt.Sprintf("/api/movie/proxy/%s/%s?t=subtitle&n=%s", movie.Movie.RoomID, movie.Movie.ID, k),
				Type: "srt",
			}
		}
		return nil

	case dbModel.StreamingVendorAlist:
		dataI, err := movie.Cache().LoadOrStore("", initAlistCache(ctx, movie.Movie), time.Minute*15)
		if err != nil {
			return err
		}

		data, ok := dataI.(*alistCache)
		if !ok {
			return errors.New("cache type error")
		}

		movie.Movie.Base.Url = data.url
		movie.Movie.Base.VendorInfo.Alist = nil
		return nil

	default:
		return fmt.Errorf("vendor not support")
	}
}
