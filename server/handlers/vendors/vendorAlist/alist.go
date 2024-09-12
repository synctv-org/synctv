package vendorAlist

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"net/http"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/synctv/utils/proxy"
	"github.com/synctv-org/vendors/api/alist"
)

type alistVendorService struct {
	room  *op.Room
	movie *op.Movie
}

func NewAlistVendorService(room *op.Room, movie *op.Movie) (*alistVendorService, error) {
	if movie.VendorInfo.Vendor != dbModel.VendorAlist {
		return nil, fmt.Errorf("alist vendor not support vendor %s", movie.MovieBase.VendorInfo.Vendor)
	}
	return &alistVendorService{
		room:  room,
		movie: movie,
	}, nil
}

func (s *alistVendorService) Client() alist.AlistHTTPServer {
	return vendor.LoadAlistClient(s.movie.VendorInfo.Backend)
}

func (s *alistVendorService) ListDynamicMovie(ctx context.Context, reqUser *op.User, subPath string, page, max int) (*model.MoviesResp, error) {
	if reqUser.ID != s.movie.CreatorID {
		return nil, fmt.Errorf("list vendor dynamic folder error: %w", dbModel.ErrNoPermission)
	}
	user := reqUser

	resp := &model.MoviesResp{
		Paths:   []*model.MoviePath{},
		Dynamic: true,
	}

	serverID, truePath, err := s.movie.VendorInfo.Alist.ServerIDAndFilePath()
	if err != nil {
		return nil, fmt.Errorf("load alist server id error: %w", err)
	}
	newPath := path.Join(truePath, subPath)
	// check new path is in parent path
	if !strings.HasPrefix(newPath, truePath) {
		return nil, fmt.Errorf("sub path is not in parent path")
	}
	truePath = newPath
	aucd, err := user.AlistCache().LoadOrStore(ctx, serverID)
	if err != nil {
		if errors.Is(err, db.ErrNotFound("vendor")) {
			return nil, errors.New("alist server not found")
		}
		return nil, err
	}
	data, err := s.Client().FsList(ctx, &alist.FsListReq{
		Token:    aucd.Token,
		Password: s.movie.VendorInfo.Alist.Password,
		Path:     truePath,
		Host:     aucd.Host,
		Refresh:  false,
		Page:     uint64(page),
		PerPage:  uint64(max),
	})
	if err != nil {
		return nil, err
	}
	resp.Total = int64(data.Total)
	resp.Movies = make([]*model.Movie, len(data.Content))
	for i, flr := range data.Content {
		resp.Movies[i] = &model.Movie{
			Id:        s.movie.ID,
			CreatedAt: s.movie.CreatedAt.UnixMilli(),
			Creator:   op.GetUserName(s.movie.CreatorID),
			CreatorId: s.movie.CreatorID,
			SubPath:   fmt.Sprintf("/%s", strings.Trim(fmt.Sprintf("%s/%s", subPath, flr.Name), "/")),
			Base: dbModel.MovieBase{
				Name:     flr.Name,
				IsFolder: flr.IsDir,
				ParentID: dbModel.EmptyNullString(s.movie.ID),
				VendorInfo: dbModel.VendorInfo{
					Vendor:  dbModel.VendorAlist,
					Backend: s.movie.VendorInfo.Backend,
					Alist: &dbModel.AlistStreamingInfo{
						Path: dbModel.FormatAlistPath(serverID, fmt.Sprintf("/%s", strings.Trim(fmt.Sprintf("%s/%s", truePath, flr.Name), "/"))),
					},
				},
			},
		}
	}
	resp.Paths = model.GenDefaultSubPaths(subPath, true, resp.Paths...)
	return resp, nil
}

func (s *alistVendorService) ProxyMovie(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)
	u, err := op.LoadOrInitUserByID(s.movie.Movie.CreatorID)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	data, err := s.movie.AlistCache().Get(ctx, &cache.AlistMovieCacheFuncArgs{
		UserCache: u.Value().AlistCache(),
		UserAgent: utils.UA,
	})
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	switch data.Provider {
	case cache.AlistProviderAli:
		t := ctx.Query("t")
		switch t {
		case "":
			ctx.Data(http.StatusOK, "audio/mpegurl", data.Ali.M3U8ListFile)
			return
		case "raw":
			err := proxy.ProxyURL(ctx, data.URL, nil)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
			}
		case "subtitle":
			idS := ctx.Query("id")
			if idS == "" {
				log.Errorf("proxy vendor movie error: %v", "id is empty")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id is empty"))
				return
			}
			id, err := strconv.Atoi(idS)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if id >= len(data.Subtitles) {
				log.Errorf("proxy vendor movie error: %v", "id out of range")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
				return
			}
			b, err := data.Subtitles[id].Cache.Get(ctx)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			http.ServeContent(ctx.Writer, ctx.Request, data.Subtitles[id].Name, time.Now(), bytes.NewReader(b))
		}

	case cache.AlistProvider115:
		fallthrough
	default:
		if !s.movie.Movie.MovieBase.Proxy {
			log.Errorf("proxy vendor movie error: %v", "not support movie proxy")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
			return
		} else {
			err = proxy.ProxyURL(ctx, data.URL, nil)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
			}
		}

	}
}

func (s *alistVendorService) GenMovieInfo(ctx context.Context, user *op.User, userAgent, userToken string) (*dbModel.Movie, error) {
	if s.movie.Proxy {
		return s.GenProxyMovieInfo(ctx, user, userAgent, userToken)
	}

	movie := s.movie.Clone()
	var err error

	creator, err := op.LoadOrInitUserByID(movie.CreatorID)
	if err != nil {
		return nil, err
	}
	alistCache := s.movie.AlistCache()
	data, err := alistCache.Get(ctx, &cache.AlistMovieCacheFuncArgs{
		UserCache: creator.Value().AlistCache(),
		UserAgent: utils.UA,
	})
	if err != nil {
		return nil, err
	}

	for _, subt := range data.Subtitles {
		if movie.MovieBase.Subtitles == nil {
			movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		}
		movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
			URL:  subt.URL,
			Type: subt.Type,
		}
	}

	switch data.Provider {
	case cache.AlistProviderAli:
		movie.MovieBase.Url = fmt.Sprintf("/api/room/%s/movie/proxy/%s?token=%s", movie.RoomID, movie.ID, userToken)
		movie.MovieBase.Type = "m3u8"

		rawStreamUrl := data.URL
		movie.MovieBase.MoreSources = []*dbModel.MoreSource{
			{
				Name: "raw",
				Type: utils.GetUrlExtension(movie.MovieBase.VendorInfo.Alist.Path),
				Url:  rawStreamUrl,
			},
		}

		for i, subt := range data.Subtitles {
			if movie.MovieBase.Subtitles == nil {
				movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
			}
			movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL:  fmt.Sprintf("/api/room/%s/movie/proxy/%s?t=subtitle&id=%d&token=%s", movie.RoomID, movie.ID, i, userToken),
				Type: subt.Type,
			}
		}

	case cache.AlistProvider115:
		data, err = alistCache.GetRefreshFunc()(ctx, &cache.AlistMovieCacheFuncArgs{
			UserCache: creator.Value().AlistCache(),
			UserAgent: userAgent,
		})
		if err != nil {
			return nil, fmt.Errorf("refresh 115 movie cache error: %w", err)
		}
		movie.MovieBase.Url = data.URL
		movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		for _, subt := range data.Subtitles {
			movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL:  subt.URL,
				Type: subt.Type,
			}
		}

	default:
		movie.MovieBase.Url = data.URL
	}

	movie.MovieBase.VendorInfo.Alist.Password = ""
	return movie, nil
}

func (s *alistVendorService) GenProxyMovieInfo(ctx context.Context, user *op.User, userAgent, userToken string) (*dbModel.Movie, error) {
	movie := s.movie.Clone()
	var err error

	creator, err := op.LoadOrInitUserByID(movie.CreatorID)
	if err != nil {
		return nil, err
	}
	alistCache := s.movie.AlistCache()
	data, err := alistCache.Get(ctx, &cache.AlistMovieCacheFuncArgs{
		UserCache: creator.Value().AlistCache(),
		UserAgent: utils.UA,
	})
	if err != nil {
		return nil, err
	}

	for _, subt := range data.Subtitles {
		if movie.MovieBase.Subtitles == nil {
			movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		}
		movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
			URL:  subt.URL,
			Type: subt.Type,
		}
	}

	switch data.Provider {
	case cache.AlistProviderAli:
		movie.MovieBase.Url = fmt.Sprintf("/api/room/%s/movie/proxy/%s?token=%s", movie.RoomID, movie.ID, userToken)
		movie.MovieBase.Type = "m3u8"

		rawStreamUrl := fmt.Sprintf("/api/room/%s/movie/proxy/%s?t=raw&token=%s", movie.RoomID, movie.ID, userToken)
		movie.MovieBase.MoreSources = []*dbModel.MoreSource{
			{
				Name: "raw",
				Type: utils.GetUrlExtension(movie.MovieBase.VendorInfo.Alist.Path),
				Url:  rawStreamUrl,
			},
		}

		for i, subt := range data.Subtitles {
			if movie.MovieBase.Subtitles == nil {
				movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
			}
			movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL:  fmt.Sprintf("/api/room/%s/movie/proxy/%s?t=subtitle&id=%d&token=%s", movie.RoomID, movie.ID, i, userToken),
				Type: subt.Type,
			}
		}

	case cache.AlistProvider115:
		movie.MovieBase.Url = fmt.Sprintf("/api/room/%s/movie/proxy/%s?token=%s", movie.RoomID, movie.ID, userToken)
		movie.MovieBase.Type = utils.GetUrlExtension(data.URL)

		// TODO: proxy subtitle

	default:
		movie.MovieBase.Url = fmt.Sprintf("/api/room/%s/movie/proxy/%s?token=%s", movie.RoomID, movie.ID, userToken)
		movie.MovieBase.Type = utils.GetUrlExtension(data.URL)
	}

	movie.MovieBase.VendorInfo.Alist.Password = ""
	return movie, nil
}
