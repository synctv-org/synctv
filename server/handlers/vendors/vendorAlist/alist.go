package vendoralist

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
	"github.com/synctv-org/synctv/server/handlers/proxy"
	"github.com/synctv-org/synctv/server/middlewares"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
)

type AlistVendorService struct {
	room  *op.Room
	movie *op.Movie
}

func NewAlistVendorService(room *op.Room, movie *op.Movie) (*AlistVendorService, error) {
	if movie.VendorInfo.Vendor != dbModel.VendorAlist {
		return nil, fmt.Errorf("alist vendor not support vendor %s", movie.VendorInfo.Vendor)
	}

	return &AlistVendorService{
		room:  room,
		movie: movie,
	}, nil
}

func (s *AlistVendorService) Client() alist.AlistHTTPServer {
	return vendor.LoadAlistClient(s.movie.VendorInfo.Backend)
}

//nolint:gosec
func (s *AlistVendorService) ListDynamicMovie(
	ctx context.Context,
	reqUser *op.User,
	subPath, keyword string,
	page, _max int,
) (*model.MovieList, error) {
	if reqUser.ID != s.movie.CreatorID {
		return nil, fmt.Errorf("list vendor dynamic folder error: %w", dbModel.ErrNoPermission)
	}

	user := reqUser

	resp := &model.MovieList{
		Paths: []*model.MoviePath{},
	}

	serverID, truePath, err := s.movie.VendorInfo.Alist.ServerIDAndFilePath()
	if err != nil {
		return nil, fmt.Errorf("load alist server id error: %w", err)
	}

	newPath := path.Join(truePath, subPath)
	// check new path is in parent path
	if !strings.HasPrefix(newPath, truePath) {
		return nil, errors.New("sub path is not in parent path")
	}

	aucd, err := user.AlistCache().LoadOrStore(ctx, serverID)
	if err != nil {
		if errors.Is(err, db.NotFoundError(db.ErrVendorNotFound)) {
			return nil, errors.New("alist server not found")
		}
		return nil, err
	}

	cli := s.Client()
	if keyword != "" {
		data, err := cli.FsSearch(ctx, &alist.FsSearchReq{
			Token:    aucd.Token,
			Password: s.movie.VendorInfo.Alist.Password,
			Parent:   newPath,
			Host:     aucd.Host,
			Page:     uint64(page),
			PerPage:  uint64(_max),
			Keywords: keyword,
		})
		if err != nil {
			return nil, err
		}

		resp.Total = int64(data.GetTotal())

		resp.Movies = make([]*model.Movie, len(data.GetContent()))
		for i, flr := range data.GetContent() {
			fileSubPath := strings.TrimPrefix(strings.Trim(flr.GetParent(), "/"), truePath)
			resp.Movies[i] = &model.Movie{
				ID:        s.movie.ID,
				CreatedAt: s.movie.CreatedAt.UnixMilli(),
				Creator:   op.GetUserName(s.movie.CreatorID),
				CreatorID: s.movie.CreatorID,
				SubPath: "/" + strings.Trim(
					fmt.Sprintf("%s/%s", fileSubPath, flr.GetName()),
					"/",
				),
				Base: dbModel.MovieBase{
					Name:     flr.GetName(),
					IsFolder: flr.GetIsDir(),
					ParentID: dbModel.EmptyNullString(s.movie.ID),
					VendorInfo: dbModel.VendorInfo{
						Vendor:  dbModel.VendorAlist,
						Backend: s.movie.VendorInfo.Backend,
						Alist: &dbModel.AlistStreamingInfo{
							Path: dbModel.FormatAlistPath(
								serverID,
								"/"+strings.Trim(
									fmt.Sprintf("%s/%s", flr.GetParent(), flr.GetName()),
									"/",
								),
							),
						},
					},
				},
			}
		}

		resp.Paths = model.GenDefaultSubPaths(s.movie.ID, subPath, true)

		return resp, nil
	}

	data, err := cli.FsList(ctx, &alist.FsListReq{
		Token:    aucd.Token,
		Password: s.movie.VendorInfo.Alist.Password,
		Path:     newPath,
		Host:     aucd.Host,
		Refresh:  false,
		Page:     uint64(page),
		PerPage:  uint64(_max),
	})
	if err != nil {
		return nil, err
	}

	resp.Total = int64(data.GetTotal())

	resp.Movies = make([]*model.Movie, len(data.GetContent()))
	for i, flr := range data.GetContent() {
		resp.Movies[i] = &model.Movie{
			ID:        s.movie.ID,
			CreatedAt: s.movie.CreatedAt.UnixMilli(),
			Creator:   op.GetUserName(s.movie.CreatorID),
			CreatorID: s.movie.CreatorID,
			SubPath:   "/" + strings.Trim(fmt.Sprintf("%s/%s", subPath, flr.GetName()), "/"),
			Base: dbModel.MovieBase{
				Name:     flr.GetName(),
				IsFolder: flr.GetIsDir(),
				ParentID: dbModel.EmptyNullString(s.movie.ID),
				VendorInfo: dbModel.VendorInfo{
					Vendor:  dbModel.VendorAlist,
					Backend: s.movie.VendorInfo.Backend,
					Alist: &dbModel.AlistStreamingInfo{
						Path: dbModel.FormatAlistPath(serverID,
							"/"+strings.Trim(fmt.Sprintf("%s/%s", newPath, flr.GetName()), "/"),
						),
					},
				},
			},
		}
	}

	resp.Paths = model.GenDefaultSubPaths(s.movie.ID, subPath, true)

	return resp, nil
}

func (s *AlistVendorService) ProxyMovie(ctx *gin.Context) {
	log := middlewares.GetLogger(ctx)

	// Get cache data
	data, err := s.getCacheData(ctx)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	// Handle different providers
	switch data.Provider {
	case cache.AlistProviderAli:
		s.handleAliProvider(ctx, log, data)
	case cache.AlistProvider115:
		fallthrough
	default:
		s.handleDefaultProvider(ctx, log, data)
	}
}

func (s *AlistVendorService) getCacheData(ctx *gin.Context) (*cache.AlistMovieCacheData, error) {
	u, err := op.LoadOrInitUserByID(s.movie.CreatorID)
	if err != nil {
		return nil, err
	}

	data, err := s.movie.AlistCache().Get(ctx, &cache.AlistMovieCacheFuncArgs{
		UserCache: u.Value().AlistCache(),
		UserAgent: utils.UA,
	})
	if err != nil {
		return nil, err
	}

	return data, nil
}

func (s *AlistVendorService) handleAliProvider(
	ctx *gin.Context,
	log *logrus.Entry,
	data *cache.AlistMovieCacheData,
) {
	t := ctx.Query("t")
	switch t {
	case "":
		b, err := data.Ali.Get(ctx)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			return
		}

		if s.movie.Proxy {
			err := proxy.M3u8Data(
				ctx,
				b.M3U8ListFile,
				"",
				ctx.GetString("token"),
				s.movie.RoomID,
				s.movie.ID,
			)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
			}
		} else {
			ctx.Data(http.StatusOK, "audio/mpegurl", b.M3U8ListFile)
		}
	case "raw":
		b, err := data.Ali.Get(ctx)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			return
		}

		if s.movie.Proxy {
			s.proxyURL(ctx, log, b.URL)
		} else {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("proxy is not enabled"))
			return
		}
	case "subtitle":
		s.handleAliSubtitle(ctx, log, data)
	}
}

func (s *AlistVendorService) handleDefaultProvider(
	ctx *gin.Context,
	log *logrus.Entry,
	data *cache.AlistMovieCacheData,
) {
	t := ctx.Query("t")
	switch t {
	case "subtitle":
		idS := ctx.Query("id")
		if idS == "" {
			log.Errorf("proxy vendor movie error: %v", "id is empty")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("id is empty"),
			)

			return
		}

		id, err := strconv.Atoi(idS)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
			return
		}

		if id >= len(data.Subtitles) {
			log.Errorf("proxy vendor movie error: %v", "id out of range")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("id out of range"),
			)

			return
		}

		subtitle := data.Subtitles[id]

		b, err := subtitle.Cache.Get(ctx)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
			return
		}

		http.ServeContent(ctx.Writer, ctx.Request, subtitle.Name, time.Now(), bytes.NewReader(b))
	default:
		if !s.movie.Proxy {
			log.Errorf("proxy vendor movie error: %v", "proxy is not enabled")
			ctx.AbortWithStatusJSON(
				http.StatusBadRequest,
				model.NewAPIErrorStringResp("proxy is not enabled"),
			)

			return
		}

		s.proxyURL(ctx, log, data.URL)
	}
}

func (s *AlistVendorService) proxyURL(ctx *gin.Context, log *logrus.Entry, url string) {
	err := proxy.AutoProxyURL(ctx,
		url,
		s.movie.Type,
		nil,
		ctx.GetString("token"),
		s.movie.RoomID,
		s.movie.ID,
		proxy.WithProxyURLCache(true),
	)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
	}
}

func (s *AlistVendorService) handleAliSubtitle(
	ctx *gin.Context,
	log *logrus.Entry,
	data *cache.AlistMovieCacheData,
) {
	idS := ctx.Query("id")
	if idS == "" {
		log.Errorf("proxy vendor movie error: %v", "id is empty")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorStringResp("id is empty"))
		return
	}

	id, err := strconv.Atoi(idS)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewAPIErrorResp(err))
		return
	}

	ali, err := data.Ali.Get(ctx)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	var subtitle *cache.AlistSubtitle
	switch {
	case id < len(data.Subtitles):
		subtitle = data.Subtitles[id]
	case id < len(data.Subtitles)+len(ali.Subtitles):
		subtitle = ali.Subtitles[id-len(data.Subtitles)]
	default:
		log.Errorf("proxy vendor movie error: %v", "id out of range")
		ctx.AbortWithStatusJSON(
			http.StatusBadRequest,
			model.NewAPIErrorStringResp("id out of range"),
		)

		return
	}

	b, err := subtitle.Cache.Get(ctx)
	if err != nil {
		log.Errorf("proxy vendor movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewAPIErrorResp(err))
		return
	}

	http.ServeContent(ctx.Writer, ctx.Request, subtitle.Name, time.Now(), bytes.NewReader(b))
}

func (s *AlistVendorService) GenMovieInfo(
	ctx context.Context,
	user *op.User,
	userAgent, userToken string,
) (*dbModel.Movie, error) {
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

	for i, subt := range data.Subtitles {
		if movie.Subtitles == nil {
			movie.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		}

		movie.Subtitles[subt.Name] = &dbModel.Subtitle{
			URL: fmt.Sprintf(
				"/api/room/movie/proxy/%s?t=subtitle&id=%d&token=%s&roomId=%s",
				movie.ID,
				i,
				userToken,
				movie.RoomID,
			),
			Type: subt.Type,
		}
	}

	switch data.Provider {
	case cache.AlistProviderAli:
		ali, err := data.Ali.Get(ctx)
		if err != nil {
			return nil, err
		}

		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = "m3u8"

		rawStreamURL := data.URL

		subPath := s.movie.SubPath()

		var rawType string
		if subPath == "" {
			rawType = utils.GetURLExtension(movie.VendorInfo.Alist.Path)
		} else {
			rawType = utils.GetURLExtension(subPath)
		}

		movie.MoreSources = []*dbModel.MoreSource{
			{
				Name: "raw",
				Type: rawType,
				URL:  rawStreamURL,
			},
		}

		for i, subt := range ali.Subtitles {
			if movie.Subtitles == nil {
				movie.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
			}

			movie.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL: fmt.Sprintf(
					"/api/room/movie/proxy/%s?t=subtitle&id=%d&token=%s&roomId=%s",
					movie.ID,
					len(data.Subtitles)+i,
					userToken,
					movie.RoomID,
				),
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

		movie.URL = data.URL

		movie.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		for _, subt := range data.Subtitles {
			movie.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL:  subt.URL,
				Type: subt.Type,
			}
		}

	default:
		movie.URL = data.URL
	}

	movie.VendorInfo.Alist.Password = ""

	return movie, nil
}

func (s *AlistVendorService) GenProxyMovieInfo(
	ctx context.Context,
	_ *op.User,
	_, userToken string,
) (*dbModel.Movie, error) {
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

	for i, subt := range data.Subtitles {
		if movie.Subtitles == nil {
			movie.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
		}

		movie.Subtitles[subt.Name] = &dbModel.Subtitle{
			URL: fmt.Sprintf(
				"/api/room/movie/proxy/%s?t=subtitle&id=%d&token=%s&roomId=%s",
				movie.ID,
				i,
				userToken,
				movie.RoomID,
			),
			Type: subt.Type,
		}
	}

	switch data.Provider {
	case cache.AlistProviderAli:
		ali, err := data.Ali.Get(ctx)
		if err != nil {
			return nil, err
		}

		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = "m3u8"

		rawStreamURL := fmt.Sprintf(
			"/api/room/movie/proxy/%s?t=raw&token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.MoreSources = []*dbModel.MoreSource{
			{
				Name: "raw",
				Type: utils.GetURLExtension(movie.VendorInfo.Alist.Path),
				URL:  rawStreamURL,
			},
		}

		for i, subt := range ali.Subtitles {
			if movie.Subtitles == nil {
				movie.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
			}

			movie.Subtitles[subt.Name] = &dbModel.Subtitle{
				URL: fmt.Sprintf(
					"/api/room/movie/proxy/%s?t=subtitle&id=%d&token=%s&roomId=%s",
					movie.ID,
					len(data.Subtitles)+i,
					userToken,
					movie.RoomID,
				),
				Type: subt.Type,
			}
		}

	case cache.AlistProvider115:
		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = utils.GetURLExtension(data.URL)

		// TODO: proxy subtitle

	default:
		movie.URL = fmt.Sprintf(
			"/api/room/movie/proxy/%s?token=%s&roomId=%s",
			movie.ID,
			userToken,
			movie.RoomID,
		)
		movie.Type = utils.GetURLExtension(data.URL)
	}

	movie.VendorInfo.Alist.Password = ""

	return movie, nil
}
