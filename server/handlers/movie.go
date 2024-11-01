package handlers

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"math/rand"
	"net/http"
	"path/filepath"
	"strings"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/server/handlers/proxy"
	"github.com/synctv-org/synctv/server/handlers/vendors"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
)

func GetPageItems[T any](ctx *gin.Context, items []T) ([]T, error) {
	page, max, err := utils.GetPageAndMax(ctx)
	if err != nil {
		return nil, err
	}

	return utils.GetPageItems(items, page, max), nil
}

func genMovieInfo(
	ctx context.Context,
	room *op.Room,
	user *op.User,
	opMovie *op.Movie,
	userAgent,
	userToken string,
) (*model.Movie, error) {
	if opMovie == nil || opMovie.ID == "" {
		return &model.Movie{}, nil
	}
	if opMovie.IsFolder {
		if !opMovie.IsDynamicFolder() {
			return nil, errors.New("movie is static folder, can't get movie info")
		}
	}
	movie := opMovie.Movie.Clone()
	if movie.MovieBase.Type == "" && movie.MovieBase.Url != "" {
		movie.MovieBase.Type = utils.GetUrlExtension(movie.MovieBase.Url)
	}
	if movie.MovieBase.VendorInfo.Vendor != "" {
		vendor, err := vendors.NewVendorService(room, opMovie)
		if err != nil {
			return nil, err
		}
		movie, err = vendor.GenMovieInfo(ctx, user, userAgent, userToken)
		if err != nil {
			return nil, err
		}
	} else if movie.MovieBase.RtmpSource {
		movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/live/hls/list/%s.m3u8?token=%s&roomId=%s", movie.ID, userToken, opMovie.RoomID)
		movie.MovieBase.Type = "m3u8"
		movie.MoreSources = append(movie.MoreSources, &dbModel.MoreSource{
			Name: "flv",
			Url:  fmt.Sprintf("/api/room/movie/live/flv/%s.flv?token=%s&roomId=%s", movie.ID, userToken, opMovie.RoomID),
			Type: "flv",
		})
		movie.MovieBase.Headers = nil
	} else if movie.MovieBase.Live && movie.MovieBase.Proxy {
		if !utils.IsM3u8Url(movie.MovieBase.Url) {
			movie.MoreSources = append(movie.MoreSources, &dbModel.MoreSource{
				Name: "flv",
				Url:  fmt.Sprintf("/api/room/movie/live/flv/%s.flv?token=%s&roomId=%s", movie.ID, userToken, opMovie.RoomID),
				Type: "flv",
			})
		}
		movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/live/hls/list/%s.m3u8?token=%s&roomId=%s", movie.ID, userToken, opMovie.RoomID)
		movie.MovieBase.Type = "m3u8"
		movie.MovieBase.Headers = nil
	} else if movie.MovieBase.Proxy {
		movie.MovieBase.Url = fmt.Sprintf("/api/room/movie/proxy/%s?token=%s&roomId=%s", movie.ID, userToken, opMovie.RoomID)
		movie.MovieBase.Headers = nil
	}
	if movie.MovieBase.Type == "" && movie.MovieBase.Url != "" {
		movie.MovieBase.Type = utils.GetUrlExtension(movie.MovieBase.Url)
	}
	for _, v := range movie.MoreSources {
		if v.Type == "" {
			v.Type = utils.GetUrlExtension(v.Url)
		}
	}
	for _, v := range movie.Subtitles {
		if v.Type == "" {
			v.Type = utils.GetUrlExtension(v.URL)
		}
	}
	resp := &model.Movie{
		Id:        movie.ID,
		CreatedAt: movie.CreatedAt.UnixMilli(),
		Base:      movie.MovieBase,
		Creator:   op.GetUserName(movie.CreatorID),
		CreatorId: movie.CreatorID,
		SubPath:   opMovie.SubPath(),
	}
	return resp, nil
}

func genCurrentRespWithCurrent(ctx context.Context, room *op.Room, user *op.User, userAgent string, userToken string) (*model.CurrentMovieResp, error) {
	current := room.Current()
	if current.Movie.ID == "" {
		return &model.CurrentMovieResp{
			Movie: &model.Movie{},
		}, nil
	}
	opMovie, err := room.GetMovieByID(current.Movie.ID)
	if err != nil {
		return nil, fmt.Errorf("get current movie error: %w", err)
	}
	mr, err := genMovieInfo(ctx, room, user, opMovie, userAgent, userToken)
	if err != nil {
		return nil, fmt.Errorf("gen current movie info error: %w", err)
	}
	resp := &model.CurrentMovieResp{
		Status:   current.UpdateStatus(),
		Movie:    mr,
		ExpireId: opMovie.ExpireId(),
	}
	return resp, nil
}

func CurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	currentResp, err := genCurrentRespWithCurrent(ctx, room, user, ctx.GetHeader("User-Agent"), ctx.GetString("token"))
	if err != nil {
		log.Errorf("gen current resp error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(currentResp))
}

func Movies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	if !user.HasRoomPermission(room, dbModel.PermissionGetMovieList) {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(dbModel.ErrNoPermission))
		return
	}

	id := ctx.Query("id")
	if len(id) != 0 && len(id) != 32 {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id length must be 0 or 32"))
		return
	}

	page, max, err := utils.GetPageAndMax(ctx)
	if err != nil {
		log.Errorf("get page and max error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if id != "" {
		mv, err := room.GetMovieByID(id)
		if err != nil {
			log.Errorf("get room movie by id error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		if !mv.IsFolder {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("parent id is not folder"))
			return
		}
		if mv.IsDynamicFolder() {
			resp, err := listVendorDynamicMovie(ctx, user, room, mv, ctx.Query("subPath"), page, max)
			if err != nil {
				log.Errorf("vendor dynamic movie list error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
			return
		}
	}

	m, total, err := user.GetRoomMoviesWithPage(room, page, max, id)
	if err != nil {
		log.Errorf("get room movies with page error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	paths, err := getParentMoviePath(room, id)
	if err != nil {
		log.Errorf("get parent movie path error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	resp := &model.MoviesResp{
		MovieList: &model.MovieList{
			Total:  total,
			Movies: make([]*model.Movie, len(m)),
			Paths:  paths,
		},
	}

	for i, v := range m {
		resp.Movies[i] = &model.Movie{
			Id:        v.ID,
			CreatedAt: v.CreatedAt.UnixMilli(),
			Base:      v.MovieBase,
			Creator:   op.GetUserName(v.CreatorID),
			CreatorId: v.CreatorID,
		}
		// hide url and headers when proxy
		if user.ID != v.CreatorID && v.MovieBase.Proxy {
			resp.Movies[i].Base.Url = ""
			resp.Movies[i].Base.Headers = nil
		}
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(resp))
}

func getParentMoviePath(room *op.Room, id string) ([]*model.MoviePath, error) {
	paths := []*model.MoviePath{
		{
			Name: "Home",
			ID:   "",
		},
	}
	if id == "" {
		return paths, nil
	}
	for id != "" {
		p, err := room.GetMovieByID(id)
		if err != nil {
			return nil, fmt.Errorf("get movie by id error: %w", err)
		}
		paths = append(paths, &model.MoviePath{
			Name: p.MovieBase.Name,
			ID:   p.ID,
		})
		id = p.ParentID.String()
	}
	return paths, nil
}

func listVendorDynamicMovie(ctx context.Context, reqUser *op.User, room *op.Room, movie *op.Movie, subPath string, page, max int) (*model.MoviesResp, error) {
	if reqUser.ID != movie.CreatorID {
		return nil, fmt.Errorf("list vendor dynamic folder error: %w", dbModel.ErrNoPermission)
	}

	paths, err := getParentMoviePath(room, movie.ID)
	if err != nil {
		return nil, fmt.Errorf("get parent movie path error: %w", err)
	}
	vendor, err := vendors.NewVendorService(room, movie)
	if err != nil {
		return nil, err
	}
	dynamic, err := vendor.ListDynamicMovie(ctx, reqUser, subPath, page, max)
	if err != nil {
		return nil, err
	}
	dynamic.Paths = append(paths, dynamic.Paths...)
	resp := &model.MoviesResp{
		MovieList: dynamic,
		Dynamic:   true,
	}
	return resp, nil
}

func PushMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	req := model.PushMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("push movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m, err := user.AddRoomMovie(room, (*dbModel.MovieBase)(&req))
	if err != nil {
		log.Errorf("push movie error: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("push movie error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(m))
}

func PushMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	req := model.PushMoviesReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var ms []*dbModel.MovieBase = make([]*dbModel.MovieBase, len(req))

	for i, v := range req {
		ms[i] = (*dbModel.MovieBase)(v)
	}

	m, err := user.AddRoomMovies(room, ms)
	if err != nil {
		log.Errorf("push movies error: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("push movies error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(m))
}

func NewPublishKey(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	if !conf.Conf.Server.Rtmp.Enable {
		log.Errorf("rtmp is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	}

	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("new publish key error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	movie, err := room.GetMovieByID(req.Id)
	if err != nil {
		log.Errorf("new publish key error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if movie.Movie.CreatorID != user.ID {
		log.Errorf("new publish key error: %v", dbModel.ErrNoPermission)
		ctx.AbortWithStatusJSON(
			http.StatusForbidden,
			model.NewApiErrorResp(
				fmt.Errorf("new publish key error: %w", dbModel.ErrNoPermission),
			),
		)
		return
	}

	if !movie.Movie.MovieBase.RtmpSource {
		log.Errorf("new publish key error: %v", "only rtmp source movie can get publish key")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("only live movie can get publish key"))
		return
	}

	token, err := rtmp.NewRtmpAuthorization(movie.Movie.ID)
	if err != nil {
		log.Errorf("new publish key error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	host := settings.CustomPublishHost.Get()
	if host == "" {
		host = HOST.Get()
	}
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
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	req := model.EditMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("edit movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.UpdateRoomMovie(room, req.Id, (*dbModel.MovieBase)(&req.PushMovieReq)); err != nil {
		log.Errorf("edit movie error: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("edit movie error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DelMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	req := model.IdsReq{}
	if err := model.Decode(ctx, &req); err != nil {
		log.Errorf("del movie error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err := user.DeleteRoomMoviesByID(room, req.Ids)
	if err != nil {
		log.Errorf("del movie error: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("del movie error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ClearMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	var req model.ClearMoviesReq
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.ClearRoomMoviesByParentID(room, req.ParentId); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("clear movies error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func SwapMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	req := model.SwapMovieReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := user.SwapRoomMoviePositions(room, req.Id1, req.Id2); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ChangeCurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*log.Entry)

	req := model.SetRoomCurrentMovieReq{}
	err := model.Decode(ctx, &req)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	err = user.SetRoomCurrentMovie(room, req.Id, req.SubPath, true)
	if err != nil {
		log.Errorf("change current movie error: %v", err)
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(
				http.StatusForbidden,
				model.NewApiErrorResp(
					fmt.Errorf("change current movie error: %w", err),
				),
			)
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ProxyMovie(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	if !settings.MovieProxy.Get() {
		log.Errorf("movie proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("movie proxy is not enabled"))
		return
	}

	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	// user := ctx.MustGet("user").(*op.UserEntry).Value()

	m, err := room.GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		log.Errorf("get movie by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if m.Movie.MovieBase.VendorInfo.Vendor != "" {
		vendor, err := vendors.NewVendorService(room, m)
		if err != nil {
			log.Errorf("get vendor service error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		vendor.ProxyMovie(ctx)
		return
	}

	if !m.Movie.MovieBase.Proxy {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("proxy is not enabled"))
		return
	}

	if m.Movie.MovieBase.Live || m.Movie.MovieBase.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("this movie is live or rtmp source, not support use this method proxy"))
		return
	}

	switch m.Movie.MovieBase.Type {
	case "mpd":
		// TODO: cache mpd file
		fallthrough
	default:
		err = proxy.AutoProxyURL(ctx, m.Movie.MovieBase.Url, m.Movie.MovieBase.Type, m.Movie.MovieBase.Headers, true, ctx.GetString("token"), room.ID, m.ID)
		if err != nil {
			log.Errorf("proxy movie error: %v", err)
			return
		}
	}
}

func ServeM3u8(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	if !settings.MovieProxy.Get() {
		log.Errorf("movie proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("movie proxy is not enabled"))
		return
	}

	room := ctx.MustGet("room").(*op.RoomEntry).Value()

	m, err := room.GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		log.Errorf("get movie by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if m.Movie.MovieBase.VendorInfo.Vendor != "" {
		err := fmt.Errorf("vendor is not supported")
		log.Errorf("get vendor service error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if m.Movie.MovieBase.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("this movie is rtmp source, not support use this method proxy"))
		return
	}

	if !m.Movie.MovieBase.Proxy {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("proxy is not enabled"))
		return
	}

	targetToken := ctx.Param("targetToken")
	claims, err := proxy.GetM3u8Target(targetToken)
	if err != nil {
		log.Errorf("auth m3u8 error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	if claims.RoomId != room.ID || claims.MovieId != m.ID {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("invalid token"))
		return
	}
	err = proxy.ProxyM3u8(ctx, claims.TargetUrl, m.Movie.MovieBase.Headers, true, claims.IsM3u8File, ctx.GetString("token"), room.ID, m.ID)
	if err != nil {
		log.Errorf("proxy m3u8 error: %v", err)
	}
}

// only cache mpd file
// func initDashCache(ctx context.Context, movie *dbModel.Movie) func() (any, error) {
// 	return func() (any, error) {
// 		req, err := http.NewRequestWithContext(ctx, http.MethodGet, movie.Base.Url, nil)
// 		if err != nil {
// 			return nil, err
// 		}
// 		for k, v := range movie.Base.Headers {
// 			req.Header.Set(k, v)
// 		}
// 		req.Header.Set("User-Agent", utils.UA)
// 		resp, err := uhc.Do(req)
// 		if err != nil {
// 			return nil, err
// 		}
// 		defer resp.Body.Close()
// 		b, err := io.ReadAll(resp.Body)
// 		if err != nil {
// 			return nil, err
// 		}
// 		m, err := mpd.ReadFromString(string(b))
// 		if err != nil {
// 			return nil, err
// 		}
// 		if len(m.BaseURL) != 0 && !path.IsAbs(m.BaseURL[0]) {
// 			result, err := url.JoinPath(path.Dir(movie.Base.Url), m.BaseURL[0])
// 			if err != nil {
// 				return nil, err
// 			}
// 			m.BaseURL = []string{result}
// 		}
// 		s, err := m.WriteToString()
// 		if err != nil {
// 			return nil, err
// 		}
// 		return s, nil
// 	}
// }

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinFlvLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".flv")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("join flv live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if !m.Movie.MovieBase.Live {
		log.Error("join hls live error: live is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live is not enabled"))
		return
	}
	if m.Movie.MovieBase.RtmpSource {
		if !conf.Conf.Server.Rtmp.Enable {
			log.Error("join hls live error: rtmp is not enabled")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
			return
		}
	} else if !settings.LiveProxy.Get() {
		log.Error("join hls live error: live proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live proxy is not enabled"))
		return
	}
	channel, err := m.Channel()
	if err != nil {
		log.Errorf("join flv live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	w := httpflv.NewHttpFLVWriter(ctx.Writer)
	defer w.Close()
	err = channel.AddPlayer(w)
	if err != nil {
		log.Errorf("join flv live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	err = w.SendPacket()
	if err != nil {
		log.Errorf("join flv live error: %v", err)
		return
	}
}

func JoinHlsLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)

	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".m3u8")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("join hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if !m.Movie.MovieBase.Live {
		log.Error("join hls live error: live is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live is not enabled"))
		return
	}
	if m.Movie.MovieBase.RtmpSource {
		if !conf.Conf.Server.Rtmp.Enable {
			log.Error("join hls live error: rtmp is not enabled")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
			return
		}
	} else if !settings.LiveProxy.Get() {
		log.Error("join hls live error: live proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live proxy is not enabled"))
		return
	}

	if utils.IsM3u8Url(m.Movie.MovieBase.Url) {
		err = proxy.ProxyM3u8(ctx, m.Movie.MovieBase.Url, m.Movie.MovieBase.Headers, true, true, ctx.GetString("token"), room.ID, m.ID)
		if err != nil {
			log.Errorf("proxy m3u8 hls live error: %v", err)
		}
		return
	}
	channel, err := m.Channel()
	if err != nil {
		log.Errorf("join hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	b, err := channel.GenM3U8File(func(tsName string) (tsPath string) {
		ext := "ts"
		if settings.TsDisguisedAsPng.Get() {
			ext = "png"
		}
		return fmt.Sprintf("/api/room/movie/live/hls/data/%s/%s/%s.%s", room.ID, movieId, tsName, ext)
	})
	if err != nil {
		log.Errorf("join hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
}

func ServeHlsLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*log.Entry)
	roomId := ctx.Param("roomId")
	roomE, err := op.LoadRoomByID(roomId)
	if err != nil {
		log.Errorf("serve hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	room := roomE.Value()

	ctx.Header("Cache-Control", "public, max-age=30, s-maxage=90")

	movieId := ctx.Param("movieId")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("serve hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if !m.Movie.MovieBase.Live {
		log.Error("join hls live error: live is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live is not enabled"))
		return
	}
	if m.Movie.MovieBase.RtmpSource {
		if !conf.Conf.Server.Rtmp.Enable {
			log.Error("join hls live error: rtmp is not enabled")
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
			return
		}
	} else if !settings.LiveProxy.Get() {
		log.Error("join hls live error: live proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live proxy is not enabled"))
		return
	}
	channel, err := m.Channel()
	if err != nil {
		log.Errorf("serve hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	dataId := ctx.Param("dataId")
	switch fileExt := filepath.Ext(dataId); fileExt {
	case ".ts":
		if settings.TsDisguisedAsPng.Get() {
			log.Errorf("serve hls live error: %v", FormatErrNotSupportFileType(fileExt))
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(strings.TrimSuffix(dataId, fileExt))
		if err != nil {
			log.Errorf("serve hls live error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Data(http.StatusOK, hls.TSContentType, b)
	case ".png":
		if !settings.TsDisguisedAsPng.Get() {
			log.Errorf("serve hls live error: %v", FormatErrNotSupportFileType(fileExt))
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(strings.TrimSuffix(dataId, fileExt))
		if err != nil {
			log.Errorf("serve hls live error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		img := image.NewGray(image.Rect(0, 0, 1, 1))
		img.Set(1, 1, color.Gray{uint8(rand.Intn(255))})
		cache := bytes.NewBuffer(make([]byte, 0, 71))
		err = png.Encode(cache, img)
		if err != nil {
			log.Errorf("serve hls live error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, "image/png", append(cache.Bytes(), b...))
	default:
		ctx.Header("Cache-Control", "no-store")
		log.Errorf("serve hls live error: %v", FormatErrNotSupportFileType(fileExt))
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
	}
}
