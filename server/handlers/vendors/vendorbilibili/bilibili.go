package vendorbilibili

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"maps"
	"net/http"
	"strconv"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/cache"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/handlers/proxy"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/bilibili"
	"github.com/zijiren233/stream"
)

type BilibiliVendorService struct {
	room  *op.Room
	movie *op.Movie
}

func NewBilibiliVendorService(room *op.Room, movie *op.Movie) (*BilibiliVendorService, error) {
	if movie.VendorInfo.Vendor != dbModel.VendorBilibili {
		return nil, fmt.Errorf("bilibili vendor not support vendor %s", movie.VendorInfo.Vendor)
	}
	return &BilibiliVendorService{
		room:  room,
		movie: movie,
	}, nil
}

func (s *BilibiliVendorService) Client() bilibili.BilibiliHTTPServer {
	return vendor.LoadBilibiliClient(s.movie.VendorInfo.Backend)
}

func (s *BilibiliVendorService) ListDynamicMovie(
	_ context.Context,
	_ *op.User,
	_, _ string,
	_, _ int,
) (*model.MovieList, error) {
	return nil, errors.New("bilibili vendor not support list dynamic movie")
}

func (s *BilibiliVendorService) ProxyMovie(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	if s.movie.Live {
		s.handleLiveProxy(ctx, log)
		return
	}

	t := ctx.Query("t")
	switch t {
	case "", "hevc":
		s.handleVideoProxy(ctx, log, t)
	case "subtitle":
		s.handleSubtitleProxy(ctx, log)
	case "danmu":
		s.handleDanmuProxy(ctx, log)
	}
}

func (s *BilibiliVendorService) handleDanmuProxy(ctx *gin.Context, log *logrus.Entry) {
	danmu, err := s.movie.BilibiliCache().Danmu.Get(ctx)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	ctx.Data(http.StatusOK, "application/xml", danmu)
}

func (s *BilibiliVendorService) handleLiveProxy(ctx *gin.Context, log *logrus.Entry) {
	data, err := s.movie.BilibiliCache().Live.Get(ctx)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	if len(data) == 0 {
		log.Error("proxy vendor movie error: live data is empty")
		ctx.AbortWithStatusJSON(
			http.StatusNotFound,
			model.NewAPIErrorStringResp("live data is empty"),
		)
		return
	}
	ctx.Data(http.StatusOK, "application/vnd.apple.mpegurl", data)
}

func (s *BilibiliVendorService) handleVideoProxy(ctx *gin.Context, log *logrus.Entry, t string) {
	if !s.movie.Proxy {
		log.Errorf("proxy vendor movie error: %v", "proxy is not enabled")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("proxy is not enabled"),
		)
		return
	}

	u, err := op.LoadOrInitUserByID(s.movie.CreatorID)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	mpdC, err := s.movie.BilibiliCache().SharedMpd.Get(ctx, u.Value().BilibiliCache())
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	id := ctx.Query("id")
	if id == "" {
		s.handleMpdProxy(ctx, log, t, mpdC)
		return
	}

	s.handleStreamProxy(ctx, log, id, mpdC)
}

func (s *BilibiliVendorService) handleMpdProxy(
	ctx *gin.Context,
	log *logrus.Entry,
	t string,
	mpdC *cache.BilibiliMpdCache,
) {
	var mpd string
	var err error
	if t == "hevc" {
		mpd, err = cache.BilibiliMpdToString(mpdC.HevcMpd, middlewares.GetToken(ctx))
	} else {
		mpd, err = cache.BilibiliMpdToString(mpdC.Mpd, middlewares.GetToken(ctx))
	}
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}
	ctx.Data(http.StatusOK, "application/dash+xml", stream.StringToBytes(mpd))
}

func (s *BilibiliVendorService) handleStreamProxy(
	ctx *gin.Context,
	log *logrus.Entry,
	id string,
	mpdC *cache.BilibiliMpdCache,
) {
	streamID, err := strconv.Atoi(id)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}
	if streamID >= len(mpdC.URLs) {
		log.Errorf("proxy vendor movie error: %v", "stream id out of range")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("stream id out of range"),
		)
		return
	}

	headers := s.getProxyHeaders()
	err = proxy.URL(ctx,
		mpdC.URLs[streamID],
		headers,
		proxy.WithProxyURLCache(true),
	)
	if err != nil {
		log.Errorf("proxy vendor movie [%s] error: %v", mpdC.URLs[streamID], err)
	}
}

func (s *BilibiliVendorService) getProxyHeaders() map[string]string {
	headers := maps.Clone(s.movie.Headers)
	if headers == nil {
		headers = map[string]string{
			"Referer":    "https://www.bilibili.com",
			"User-Agent": utils.UA,
		}
	} else {
		headers["Referer"] = "https://www.bilibili.com"
		headers["User-Agent"] = utils.UA
	}
	return headers
}

func (s *BilibiliVendorService) handleSubtitleProxy(ctx *gin.Context, log *logrus.Entry) {
	id := ctx.Query("n")
	if id == "" {
		log.Errorf("proxy vendor movie error: %v", "n is empty")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("n is empty"))
		return
	}

	u, err := op.LoadOrInitUserByID(s.movie.CreatorID)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	srtI, err := s.movie.BilibiliCache().Subtitle.Get(ctx, u.Value().BilibiliCache())
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	if s, ok := srtI[id]; ok {
		srtData, err := s.Srt.Get(ctx)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			return
		}
		http.ServeContent(ctx.Writer, ctx.Request, id, time.Now(), bytes.NewReader(srtData))
		return
	}

	log.Errorf("proxy vendor movie error: %v", "subtitle not found")
	ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewAPIErrorStringResp("subtitle not found"))
}

func (s *BilibiliVendorService) GenMovieInfo(
	ctx context.Context,
	user *op.User,
	userAgent, userToken string,
) (*dbModel.Movie, error) {
	if s.movie.Proxy {
		return s.GenProxyMovieInfo(ctx, user, userAgent, userToken)
	}

	movie := s.movie.Clone()
	var err error
	if movie.IsFolder {
		return nil, errors.New("bilibili folder not support")
	}

	bmc := s.movie.BilibiliCache()
	if movie.Live {
		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = "m3u8"

		movie.StreamDanmu = fmt.Sprintf(
			"/api/room/movie/danmu/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		return movie, nil
	}

	movie.Danmu = fmt.Sprintf(
		"/api/room/movie/proxy/%s?token=%s&t=danmu&roomId=%s",
		movie.ID,
		userToken,
		movie.RoomID,
	)

	var str string
	if movie.VendorInfo.Bilibili.Shared {
		var u *op.UserEntry
		u, err = op.LoadOrInitUserByID(movie.CreatorID)
		if err != nil {
			return nil, err
		}
		str, err = s.movie.BilibiliCache().NoSharedMovie.LoadOrStore(
			ctx,
			movie.CreatorID,
			u.Value().BilibiliCache(),
		)
	} else {
		str, err = s.movie.BilibiliCache().NoSharedMovie.LoadOrStore(ctx, user.ID, user.BilibiliCache())
	}
	if err != nil {
		return nil, err
	}
	movie.URL = str

	srt, err := bmc.Subtitle.Get(ctx, user.BilibiliCache())
	if err != nil {
		return nil, err
	}
	for k := range srt {
		if movie.Subtitles == nil {
			movie.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
		}
		movie.Subtitles[k] = &dbModel.Subtitle{
			URL: fmt.Sprintf(
				"/api/room/movie/proxy/%s?t=subtitle&n=%s&token=%s&roomId=%s",
				movie.ID,
				k,
				userToken,
				movie.RoomID,
			),
			Type: "srt",
		}
	}
	return movie, nil
}

func (s *BilibiliVendorService) GenProxyMovieInfo(
	ctx context.Context,
	user *op.User,
	_, userToken string,
) (*dbModel.Movie, error) {
	movie := s.movie.Clone()
	var err error
	if movie.IsFolder {
		return nil, errors.New("bilibili folder not support")
	}

	bmc := s.movie.BilibiliCache()
	if movie.Live {
		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = "m3u8"

		movie.StreamDanmu = fmt.Sprintf(
			"/api/room/movie/danmu/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		return movie, nil
	}

	movie.Danmu = fmt.Sprintf(
		"/api/room/movie/proxy/%s?token=%s&t=danmu&roomId=%s",
		movie.ID,
		userToken,
		movie.RoomID,
	)

	movie.URL = fmt.Sprintf(
		"/api/room/movie/proxy/%s?token=%s&roomId=%s",
		movie.ID,
		userToken,
		movie.RoomID,
	)
	movie.Type = "mpd"
	movie.MoreSources = []*dbModel.MoreSource{
		{
			Name: "hevc",
			Type: "mpd",
			URL: fmt.Sprintf(
				"/api/room/movie/proxy/%s?token=%s&t=hevc&roomId=%s",
				movie.ID,
				userToken,
				movie.RoomID,
			),
		},
	}
	srt, err := bmc.Subtitle.Get(ctx, user.BilibiliCache())
	if err != nil {
		return nil, err
	}
	for k := range srt {
		if movie.Subtitles == nil {
			movie.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
		}
		movie.Subtitles[k] = &dbModel.Subtitle{
			URL: fmt.Sprintf(
				"/api/room/movie/proxy/%s?t=subtitle&n=%s&token=%s&roomId=%s",
				movie.ID,
				k,
				userToken,
				movie.RoomID,
			),
			Type: "srt",
		}
	}
	return movie, nil
}
