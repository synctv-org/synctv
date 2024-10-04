package vendorBilibili

import (
	"bytes"
	"context"
	"fmt"
	"net/http"
	"strconv"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/cache"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/synctv/utils/proxy"
	"github.com/synctv-org/vendors/api/bilibili"
	"github.com/zijiren233/stream"
	"golang.org/x/exp/maps"
)

type bilibiliVendorService struct {
	room  *op.Room
	movie *op.Movie
}

func NewBilibiliVendorService(room *op.Room, movie *op.Movie) (*bilibiliVendorService, error) {
	if movie.VendorInfo.Vendor != dbModel.VendorBilibili {
		return nil, fmt.Errorf("bilibili vendor not support vendor %s", movie.MovieBase.VendorInfo.Vendor)
	}
	return &bilibiliVendorService{
		room:  room,
		movie: movie,
	}, nil
}

func (s *bilibiliVendorService) Client() bilibili.BilibiliHTTPServer {
	return vendor.LoadBilibiliClient(s.movie.VendorInfo.Backend)
}

func (s *bilibiliVendorService) ListDynamicMovie(ctx context.Context, reqUser *op.User, subPath string, page, max int) (*model.MoviesResp, error) {
	return nil, fmt.Errorf("bilibili vendor not support list dynamic movie")
}

func (s *bilibiliVendorService) ProxyMovie(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	if s.movie.MovieBase.Live {
		data, err := s.movie.BilibiliCache().Live.Get(ctx)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		if len(data) == 0 {
			log.Error("proxy vendor movie error: live data is empty")
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorStringResp("live data is empty"))
			return
		}
		ctx.Data(http.StatusOK, "application/vnd.apple.mpegurl", data)
		return
	}

	t := ctx.Query("t")
	switch t {
	case "", "hevc":
		if !s.movie.Movie.MovieBase.Proxy {
			log.Errorf("proxy vendor movie error: %v", "not support movie proxy")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
			return
		}
		u, err := op.LoadOrInitUserByID(s.movie.Movie.CreatorID)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		mpdC, err := s.movie.BilibiliCache().SharedMpd.Get(ctx, u.Value().BilibiliCache())
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		id := ctx.Query("id")
		if id == "" {
			if t == "hevc" {
				s, err := cache.BilibiliMpdToString(mpdC.HevcMpd, ctx.MustGet("token").(string))
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				ctx.Data(http.StatusOK, "application/dash+xml", stream.StringToBytes(s))
			} else {
				s, err := cache.BilibiliMpdToString(mpdC.Mpd, ctx.MustGet("token").(string))
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				ctx.Data(http.StatusOK, "application/dash+xml", stream.StringToBytes(s))
			}
			return
		}

		streamId, err := strconv.Atoi(id)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		if streamId >= len(mpdC.Urls) {
			log.Errorf("proxy vendor movie error: %v", "stream id out of range")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("stream id out of range"))
			return
		}
		headers := maps.Clone(s.movie.Movie.MovieBase.Headers)
		if headers == nil {
			headers = map[string]string{
				"Referer":    "https://www.bilibili.com",
				"User-Agent": utils.UA,
			}
		} else {
			headers["Referer"] = "https://www.bilibili.com"
			headers["User-Agent"] = utils.UA
		}
		err = proxy.ProxyURL(ctx, mpdC.Urls[streamId], headers)
		if err != nil {
			log.Errorf("proxy vendor movie [%s] error: %v", mpdC.Urls[streamId], err)
		}
	case "subtitle":
		id := ctx.Query("n")
		if id == "" {
			log.Errorf("proxy vendor movie error: %v", "n is empty")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("n is empty"))
			return
		}
		u, err := op.LoadOrInitUserByID(s.movie.Movie.CreatorID)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		srtI, err := s.movie.BilibiliCache().Subtitle.Get(ctx, u.Value().BilibiliCache())
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		if s, ok := srtI[id]; ok {
			srtData, err := s.Srt.Get(ctx)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			http.ServeContent(ctx.Writer, ctx.Request, id, time.Now(), bytes.NewReader(srtData))
			return
		} else {
			log.Errorf("proxy vendor movie error: %v", "subtitle not found")
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorStringResp("subtitle not found"))
			return
		}
	}
}

func (s *bilibiliVendorService) GenMovieInfo(ctx context.Context, user *op.User, userAgent, userToken string) (*dbModel.Movie, error) {
	if s.movie.Proxy {
		return s.GenProxyMovieInfo(ctx, user, userAgent, userToken)
	}

	movie := s.movie.Clone()
	var err error
	if movie.IsFolder {
		return nil, fmt.Errorf("bilibili folder not support")
	}

	bmc := s.movie.BilibiliCache()
	if movie.MovieBase.Live {
		movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/proxy/%s?token=%s&roomId=%s", movie.ID, userToken, movie.RoomID)
		movie.MovieBase.Type = "m3u8"
		return movie, nil
	}

	var str string
	if movie.MovieBase.VendorInfo.Bilibili.Shared {
		var u *op.UserEntry
		u, err = op.LoadOrInitUserByID(movie.CreatorID)
		if err != nil {
			return nil, err
		}
		str, err = s.movie.BilibiliCache().NoSharedMovie.LoadOrStore(ctx, movie.CreatorID, u.Value().BilibiliCache())
	} else {
		str, err = s.movie.BilibiliCache().NoSharedMovie.LoadOrStore(ctx, user.ID, user.BilibiliCache())
	}
	if err != nil {
		return nil, err
	}
	movie.MovieBase.Url = str

	srt, err := bmc.Subtitle.Get(ctx, user.BilibiliCache())
	if err != nil {
		return nil, err
	}
	for k := range srt {
		if movie.MovieBase.Subtitles == nil {
			movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
		}
		movie.MovieBase.Subtitles[k] = &dbModel.Subtitle{
			URL:  fmt.Sprintf("/api/room/movie/proxy/%s?t=subtitle&n=%s&token=%s&roomId=%s", movie.ID, k, userToken, movie.RoomID),
			Type: "srt",
		}
	}
	return movie, nil
}

func (s *bilibiliVendorService) GenProxyMovieInfo(ctx context.Context, user *op.User, userAgent, userToken string) (*dbModel.Movie, error) {
	movie := s.movie.Clone()
	var err error
	if movie.IsFolder {
		return nil, fmt.Errorf("bilibili folder not support")
	}

	bmc := s.movie.BilibiliCache()
	if movie.MovieBase.Live {
		movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/proxy/%s?token=%s&roomId=%s", movie.ID, userToken, movie.RoomID)
		movie.MovieBase.Type = "m3u8"
		return movie, nil
	}

	movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/proxy/%s?token=%s&roomId=%s", movie.ID, userToken, movie.RoomID)
	movie.MovieBase.Type = "mpd"
	movie.MovieBase.MoreSources = []*dbModel.MoreSource{
		{
			Name: "hevc",
			Type: "mpd",
			Url:  fmt.Sprintf("/api/room/movie/proxy/%s?token=%s&t=hevc&roomId=%s", movie.ID, userToken, movie.RoomID),
		},
	}
	srt, err := bmc.Subtitle.Get(ctx, user.BilibiliCache())
	if err != nil {
		return nil, err
	}
	for k := range srt {
		if movie.MovieBase.Subtitles == nil {
			movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
		}
		movie.MovieBase.Subtitles[k] = &dbModel.Subtitle{
			URL:  fmt.Sprintf("/api/room/movie/proxy/%s?t=subtitle&n=%s&token=%s&roomId=%s", movie.ID, k, userToken, movie.RoomID),
			Type: "srt",
		}
	}
	return movie, nil
}
