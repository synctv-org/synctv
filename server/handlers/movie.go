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
	"math/rand"
	"net/http"
	"net/url"
	"path"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/cache"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	"github.com/synctv-org/synctv/internal/vendor"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/vendors/api/alist"
	"github.com/synctv-org/vendors/api/emby"
	uhc "github.com/zijiren233/go-uhc"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
	"github.com/zijiren233/stream"
	"golang.org/x/exp/maps"
)

func GetPageItems[T any](ctx *gin.Context, items []T) ([]T, error) {
	page, max, err := utils.GetPageAndMax(ctx)
	if err != nil {
		return nil, err
	}

	return utils.GetPageItems(items, page, max), nil
}

// func MovieList(ctx *gin.Context) {
// 	room := ctx.MustGet("room").(*op.RoomEntry).Value()
// 	user := ctx.MustGet("user").(*op.UserEntry).Value()
// 	log := ctx.MustGet("log").(*logrus.Entry)

// 	page, max, err := utils.GetPageAndMax(ctx)
// 	if err != nil {
// 		log.Errorf("get page and max error: %v", err)
// 		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
// 		return
// 	}

// 	currentResp, err := genCurrentResp(ctx, user, room)
// 	if err != nil {
// 		log.Errorf("gen current resp error: %v", err)
// 		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
// 		return
// 	}

// 	m := room.GetMoviesWithPage(page, max)
// 	mresp := make([]model.MovieResp, len(m))
// 	for i, v := range m {
// 		mresp[i] = model.MovieResp{
// 			Id:      v.Movie.ID,
// 			Base:    v.Movie.Base,
// 			Creator: op.GetUserName(v.Movie.CreatorID),
// 		}
// 		// hide url and headers when proxy
// 		if user.ID != v.Movie.CreatorID && v.Movie.Base.Proxy {
// 			mresp[i].Base.Url = ""
// 			mresp[i].Base.Headers = nil
// 		}
// 	}

// 	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
// 		"current": currentResp,
// 		"total":   room.GetMoviesCount(),
// 		"movies":  mresp,
// 	}))
// }

func genMovieInfo(
	ctx context.Context,
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
	var movie = opMovie.Movie.Clone()
	if movie.MovieBase.VendorInfo.Vendor != "" {
		vendorMovie, err := genVendorMovie(ctx, user, opMovie, userAgent, userToken)
		if err != nil {
			return nil, err
		}
		movie = vendorMovie
	} else if movie.MovieBase.RtmpSource || movie.MovieBase.Live && movie.MovieBase.Proxy {
		movie.MovieBase.Url = fmt.Sprintf("/api/movie/live/hls/list/%s.m3u8?token=%s", movie.ID, userToken)
		movie.MovieBase.Type = "m3u8"
		movie.MoreSources = append(movie.MoreSources, &dbModel.MoreSource{
			Name: "flv",
			Url:  fmt.Sprintf("/api/movie/live/flv/%s.flv?token=%s", movie.ID, userToken),
			Type: "flv",
		})
		movie.MovieBase.Headers = nil
	} else if movie.MovieBase.Proxy {
		movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
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

func genCurrentRespWithCurrent(ctx context.Context, user *op.User, room *op.Room, userAgent string, userToken string) (*model.CurrentMovieResp, error) {
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
	mr, err := genMovieInfo(ctx, user, opMovie, userAgent, userToken)
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
	log := ctx.MustGet("log").(*logrus.Entry)

	currentResp, err := genCurrentRespWithCurrent(ctx, user, room, ctx.GetHeader("User-Agent"), ctx.MustGet("token").(string))
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
	log := ctx.MustGet("log").(*logrus.Entry)

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
			resp, err := listVendorDynamicMovie(ctx, user, room, mv.Movie, ctx.Query("subPath"), page, max)
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
		Total:  total,
		Movies: make([]*model.Movie, len(m)),
		Paths:  paths,
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

func listVendorDynamicMovie(ctx context.Context, reqUser *op.User, room *op.Room, movie *dbModel.Movie, subPath string, page, max int) (*model.MoviesResp, error) {
	if reqUser.ID != movie.CreatorID {
		return nil, fmt.Errorf("list vendor dynamic folder error: %w", dbModel.ErrNoPermission)
	}
	// creatorE, err := op.LoadOrInitUserByID(movie.CreatorID)
	// if err != nil {
	// 	return nil, err
	// }
	user := reqUser

	paths, err := getParentMoviePath(room, movie.ID)
	if err != nil {
		return nil, fmt.Errorf("get parent movie path error: %w", err)
	}
	resp := &model.MoviesResp{
		Paths:   paths,
		Dynamic: true,
	}

	switch movie.MovieBase.VendorInfo.Vendor {
	case dbModel.VendorAlist:
		serverID, truePath, err := movie.VendorInfo.Alist.ServerIDAndFilePath()
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
		var cli = vendor.LoadAlistClient(movie.VendorInfo.Backend)
		data, err := cli.FsList(ctx, &alist.FsListReq{
			Token:    aucd.Token,
			Password: movie.VendorInfo.Alist.Password,
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
				Id:        movie.ID,
				CreatedAt: movie.CreatedAt.UnixMilli(),
				Creator:   op.GetUserName(movie.CreatorID),
				CreatorId: movie.CreatorID,
				SubPath:   fmt.Sprintf("/%s", strings.Trim(fmt.Sprintf("%s/%s", subPath, flr.Name), "/")),
				Base: dbModel.MovieBase{
					Name:     flr.Name,
					IsFolder: flr.IsDir,
					ParentID: dbModel.EmptyNullString(movie.ID),
					VendorInfo: dbModel.VendorInfo{
						Vendor:  dbModel.VendorAlist,
						Backend: movie.VendorInfo.Backend,
						Alist: &dbModel.AlistStreamingInfo{
							Path: dbModel.FormatAlistPath(serverID, fmt.Sprintf("/%s", strings.Trim(fmt.Sprintf("%s/%s", truePath, flr.Name), "/"))),
						},
					},
				},
			}
		}
		resp.Paths = model.GenDefaultSubPaths(subPath, true, resp.Paths...)

	case dbModel.VendorEmby:
		serverID, truePath, err := movie.VendorInfo.Emby.ServerIDAndFilePath()
		if err != nil {
			return nil, fmt.Errorf("load emby server id error: %w", err)
		}
		if subPath != "" {
			truePath = subPath
		}
		aucd, err := user.EmbyCache().LoadOrStore(ctx, serverID)
		if err != nil {
			if errors.Is(err, db.ErrNotFound("vendor")) {
				return nil, errors.New("emby server not found")
			}
			return nil, err
		}
		var cli = vendor.LoadEmbyClient(movie.VendorInfo.Backend)
		data, err := cli.FsList(ctx, &emby.FsListReq{
			Host:       aucd.Host,
			Path:       truePath,
			Token:      aucd.ApiKey,
			UserId:     aucd.UserID,
			Limit:      uint64(max),
			StartIndex: uint64((page - 1) * max),
		})
		if err != nil {
			return nil, fmt.Errorf("emby fs list error: %w", err)
		}
		resp.Total = int64(data.Total)
		resp.Movies = make([]*model.Movie, len(data.Items))
		for i, flr := range data.Items {
			resp.Movies[i] = &model.Movie{
				Id:        movie.ID,
				CreatedAt: movie.CreatedAt.UnixMilli(),
				Creator:   op.GetUserName(movie.CreatorID),
				CreatorId: movie.CreatorID,
				SubPath:   flr.Id,
				Base: dbModel.MovieBase{
					Name:     flr.Name,
					IsFolder: flr.IsFolder,
					ParentID: dbModel.EmptyNullString(movie.ID),
					VendorInfo: dbModel.VendorInfo{
						Vendor:  dbModel.VendorEmby,
						Backend: movie.VendorInfo.Backend,
						Emby: &dbModel.EmbyStreamingInfo{
							Path: dbModel.FormatEmbyPath(serverID, flr.Id),
						},
					},
				},
			}
		}

	default:
		return nil, fmt.Errorf("%v vendor not implement list dynamic movie", movie.MovieBase.VendorInfo.Vendor)
	}

	return resp, nil
}

func PushMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

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
	log := ctx.MustGet("log").(*logrus.Entry)

	if !settings.MovieProxy.Get() {
		log.Errorf("movie proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("movie proxy is not enabled"))
		return
	}
	roomId := ctx.Param("roomId")
	if roomId == "" {
		log.Errorf("room id is empty")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("roomId is empty"))
		return
	}

	room, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		log.Errorf("load or init room by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m, err := room.Value().GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		log.Errorf("get movie by id error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if m.Movie.MovieBase.VendorInfo.Vendor != "" {
		proxyVendorMovie(ctx, m)
		return
	}

	if !m.Movie.MovieBase.Proxy || m.Movie.MovieBase.Live || m.Movie.MovieBase.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
		return
	}

	switch m.Movie.MovieBase.Type {
	case "mpd":
		// TODO: cache mpd file
		fallthrough
	default:
		err = proxyURL(ctx, m.Movie.MovieBase.Url, m.Movie.MovieBase.Headers)
		if err != nil {
			log.Errorf("proxy movie error: %v", err)
			return
		}
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

func proxyURL(ctx *gin.Context, u string, headers map[string]string) error {
	if utils.GetUrlExtension(u) == "m3u8" {
		ctx.Redirect(http.StatusFound, u)
		return nil
	}
	if !settings.AllowProxyToLocal.Get() {
		if l, err := utils.ParseURLIsLocalIP(u); err != nil {
			return fmt.Errorf("check url is local ip error: %w", err)
		} else if l {
			return errors.New("not allow proxy to local")
		}
	}
	ctx2, cf := context.WithCancel(ctx)
	defer cf()
	req, err := http.NewRequestWithContext(ctx2, http.MethodGet, u, nil)
	if err != nil {
		return fmt.Errorf("new request error: %w", err)
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	req.Header.Set("Range", ctx.GetHeader("Range"))
	req.Header.Set("Accept-Encoding", ctx.GetHeader("Accept-Encoding"))
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	resp, err := uhc.Do(req)
	if err != nil {
		return fmt.Errorf("request url error: %w", err)
	}
	defer resp.Body.Close()
	ctx.Status(resp.StatusCode)
	ctx.Header("Accept-Ranges", resp.Header.Get("Accept-Ranges"))
	ctx.Header("Cache-Control", resp.Header.Get("Cache-Control"))
	ctx.Header("Content-Length", resp.Header.Get("Content-Length"))
	ctx.Header("Content-Range", resp.Header.Get("Content-Range"))
	ctx.Header("Content-Type", resp.Header.Get("Content-Type"))
	_, err = io.Copy(ctx.Writer, resp.Body)
	if err != nil && err != io.EOF {
		return fmt.Errorf("copy response body error: %w", err)
	}
	return nil
}

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	token := ctx.MustGet("token").(string)

	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.Trim(ctx.Param("movieId"), "/")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("join live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if m.Movie.MovieBase.RtmpSource && !conf.Conf.Server.Rtmp.Enable {
		log.Error("join live error: rtmp is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	} else if m.Movie.MovieBase.Live && !settings.LiveProxy.Get() {
		log.Error("join live error: live proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live proxy is not enabled"))
		return
	}
	channel, err := m.Channel()
	if err != nil {
		log.Errorf("join live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}

	joinType := ctx.DefaultQuery("type", "auto")
	if joinType == "auto" {
		joinType = m.Movie.MovieBase.Type
	}
	switch joinType {
	case "flv":
		w := httpflv.NewHttpFLVWriter(ctx.Writer)
		defer w.Close()
		err = channel.AddPlayer(w)
		if err != nil {
			log.Errorf("join live error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		_ = w.SendPacket()
	case "m3u8":
		b, err := channel.GenM3U8File(func(tsName string) (tsPath string) {
			ext := "ts"
			if settings.TsDisguisedAsPng.Get() {
				ext = "png"
			}
			return fmt.Sprintf("/api/movie/live/hls/data/%s/%s/%s.%s?token=%s", room.ID, movieId, tsName, ext, token)
		})
		if err != nil {
			log.Errorf("join live error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
	default:
		log.Errorf("join live error: %v", FormatErrNotSupportFileType(joinType))
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(fmt.Sprintf("not support join type: %s", joinType)))
		return
	}
}

func JoinFlvLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".flv")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("join flv live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if m.Movie.MovieBase.RtmpSource && !conf.Conf.Server.Rtmp.Enable {
		log.Error("join flv live error: rtmp is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	} else if m.Movie.MovieBase.Live && !settings.LiveProxy.Get() {
		log.Error("join flv live error: live proxy is not enabled")
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
	_ = w.SendPacket()
}

func JoinHlsLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)
	token := ctx.MustGet("token").(string)

	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".m3u8")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("join hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if m.Movie.MovieBase.RtmpSource && !conf.Conf.Server.Rtmp.Enable {
		log.Error("join hls live error: rtmp is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	} else if m.Movie.MovieBase.Live && !settings.LiveProxy.Get() {
		log.Error("join hls live error: live proxy is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("live proxy is not enabled"))
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
		return fmt.Sprintf("/api/movie/live/hls/data/%s/%s/%s.%s?token=%s", room.ID, movieId, tsName, ext, token)
	})
	if err != nil {
		log.Errorf("join hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
}

func ServeHlsLive(ctx *gin.Context) {
	log := ctx.MustGet("log").(*logrus.Entry)

	ctx.Header("Cache-Control", "no-store")
	roomId := ctx.Param("roomId")
	roomE, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		log.Errorf("serve hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	room := roomE.Value()
	movieId := ctx.Param("movieId")
	m, err := room.GetMovieByID(movieId)
	if err != nil {
		log.Errorf("serve hls live error: %v", err)
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	if m.Movie.MovieBase.RtmpSource && !conf.Conf.Server.Rtmp.Enable {
		log.Error("serve hls live error: rtmp is not enabled")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("rtmp is not enabled"))
		return
	} else if m.Movie.MovieBase.Live && !settings.LiveProxy.Get() {
		log.Error("serve hls live error: live proxy is not enabled")
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

func proxyVendorMovie(ctx *gin.Context, movie *op.Movie) {
	log := ctx.MustGet("log").(*logrus.Entry)

	switch movie.Movie.MovieBase.VendorInfo.Vendor {
	case dbModel.VendorBilibili:
		if movie.MovieBase.Live {
			data, err := movie.BilibiliCache().Live.Get(ctx)
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
		} else {
			t := ctx.Query("t")
			switch t {
			case "", "hevc":
				if !movie.Movie.MovieBase.Proxy {
					log.Errorf("proxy vendor movie error: %v", "not support movie proxy")
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
					return
				}
				u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				mpdC, err := movie.BilibiliCache().SharedMpd.Get(ctx, u.Value().BilibiliCache())
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				if id := ctx.Query("id"); id == "" {
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
				} else {
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
					headers := maps.Clone(movie.Movie.MovieBase.Headers)
					if headers == nil {
						headers = map[string]string{
							"Referer":    "https://www.bilibili.com",
							"User-Agent": utils.UA,
						}
					} else {
						headers["Referer"] = "https://www.bilibili.com"
						headers["User-Agent"] = utils.UA
					}
					err = proxyURL(ctx, mpdC.Urls[streamId], headers)
					if err != nil {
						log.Errorf("proxy vendor movie [%s] error: %v", mpdC.Urls[streamId], err)
					}
					return
				}
			case "subtitle":
				id := ctx.Query("n")
				if id == "" {
					log.Errorf("proxy vendor movie error: %v", "n is empty")
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("n is empty"))
					return
				}
				u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				srtI, err := movie.BilibiliCache().Subtitle.Get(ctx, u.Value().BilibiliCache())
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

	case dbModel.VendorAlist:
		u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
		if err != nil {
			log.Errorf("proxy vendor movie error: %v", err)
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		data, err := movie.AlistCache().Get(ctx, &cache.AlistMovieCacheFuncArgs{
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
			if !movie.Movie.MovieBase.Proxy {
				log.Errorf("proxy vendor movie error: %v", "not support movie proxy")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
				return
			} else {
				err = proxyURL(ctx, data.URL, nil)
				if err != nil {
					log.Errorf("proxy vendor movie error: %v", err)
				}
			}

		}
		return

	case dbModel.VendorEmby:
		t := ctx.Query("t")
		switch t {
		case "":
			if !movie.Movie.MovieBase.Proxy {
				log.Errorf("proxy vendor movie error: %v", "not support movie proxy")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
				return
			}
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			embyC, err := movie.EmbyCache().Get(ctx, u.Value().EmbyCache())
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			source, err := strconv.Atoi(ctx.Query("source"))
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if source >= len(embyC.Sources) {
				log.Errorf("proxy vendor movie error: %v", "source out of range")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("source out of range"))
				return
			}
			id, err := strconv.Atoi(ctx.Query("source"))
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if id >= len(embyC.Sources[source].URL) {
				log.Errorf("proxy vendor movie error: %v", "id out of range")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
				return
			}
			if embyC.Sources[source].IsTranscode {
				ctx.Redirect(http.StatusFound, embyC.Sources[source].URL)
				return
			}
			err = proxyURL(ctx, embyC.Sources[source].URL, nil)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
			}
			return

		case "subtitle":
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			embyC, err := movie.EmbyCache().Get(ctx, u.Value().EmbyCache())
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			source, err := strconv.Atoi(ctx.Query("source"))
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if source >= len(embyC.Sources) {
				log.Errorf("proxy vendor movie error: %v", "source out of range")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("source out of range"))
				return
			}
			id, err := strconv.Atoi(ctx.Query("id"))
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if id >= len(embyC.Sources[source].Subtitles) {
				log.Errorf("proxy vendor movie error: %v", "id out of range")
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
				return
			}
			data, err := embyC.Sources[source].Subtitles[id].Cache.Get(ctx)
			if err != nil {
				log.Errorf("proxy vendor movie error: %v", err)
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			http.ServeContent(ctx.Writer, ctx.Request, embyC.Sources[source].Subtitles[id].Name, time.Now(), bytes.NewReader(data))
			return
		}

	default:
		log.Errorf("proxy vendor movie error: %v", "vendor not support proxy")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("vendor not support proxy"))
		return
	}
}

// user is the api requester
func genVendorMovie(ctx context.Context, user *op.User, opMovie *op.Movie, userAgent, userToken string) (*dbModel.Movie, error) {
	movie := *opMovie.Movie
	var err error
	switch movie.MovieBase.VendorInfo.Vendor {
	case dbModel.VendorBilibili:
		if movie.IsFolder {
			return nil, fmt.Errorf("bilibili folder not support")
		}

		bmc := opMovie.BilibiliCache()
		if movie.MovieBase.Live {
			movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
			movie.MovieBase.Type = "m3u8"
			return &movie, nil
		} else {
			if !movie.MovieBase.Proxy {
				var s string
				if movie.MovieBase.VendorInfo.Bilibili.Shared {
					var u *op.UserEntry
					u, err = op.LoadOrInitUserByID(movie.CreatorID)
					if err != nil {
						return nil, err
					}
					s, err = opMovie.BilibiliCache().NoSharedMovie.LoadOrStore(ctx, movie.CreatorID, u.Value().BilibiliCache())
				} else {
					s, err = opMovie.BilibiliCache().NoSharedMovie.LoadOrStore(ctx, user.ID, user.BilibiliCache())
				}
				if err != nil {
					return nil, err
				}

				movie.MovieBase.Url = s
			} else {
				movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
				movie.MovieBase.Type = "mpd"
				movie.MovieBase.MoreSources = []*dbModel.MoreSource{
					{
						Name: "hevc",
						Type: "mpd",
						Url:  fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s&t=hevc", movie.RoomID, movie.ID, userToken),
					},
				}
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
					URL:  fmt.Sprintf("/api/movie/proxy/%s/%s?t=subtitle&n=%s&token=%s", movie.RoomID, movie.ID, k, userToken),
					Type: "srt",
				}
			}
			return &movie, nil
		}

	case dbModel.VendorAlist:
		creator, err := op.LoadOrInitUserByID(movie.CreatorID)
		if err != nil {
			return nil, err
		}
		alistCache := opMovie.AlistCache()
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
			movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
			movie.MovieBase.Type = "m3u8"

			for i, subt := range data.Subtitles {
				if movie.MovieBase.Subtitles == nil {
					movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Subtitles))
				}
				movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
					URL:  fmt.Sprintf("/api/movie/proxy/%s/%s?t=subtitle&id=%d&token=%s", movie.RoomID, movie.ID, i, userToken),
					Type: subt.Type,
				}
			}

		case cache.AlistProvider115:
			if movie.MovieBase.Proxy {
				movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
				movie.MovieBase.Type = utils.GetUrlExtension(data.URL)

				// TODO: proxy subtitle
			} else {
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
			}

		default:
			if !movie.MovieBase.Proxy {
				movie.MovieBase.Url = data.URL
			} else {
				movie.MovieBase.Url = fmt.Sprintf("/api/movie/proxy/%s/%s?token=%s", movie.RoomID, movie.ID, userToken)
				movie.MovieBase.Type = utils.GetUrlExtension(data.URL)
			}
		}

		movie.MovieBase.VendorInfo.Alist.Password = ""
		return &movie, nil

	case dbModel.VendorEmby:
		u, err := op.LoadOrInitUserByID(movie.CreatorID)
		if err != nil {
			return nil, err
		}
		data, err := opMovie.EmbyCache().Get(ctx, u.Value().EmbyCache())
		if err != nil {
			return nil, err
		}

		if !movie.MovieBase.Proxy {
			if len(data.Sources) == 0 {
				return nil, errors.New("no source")
			}
			movie.MovieBase.Url = data.Sources[0].URL
			for _, s := range data.Sources[0].Subtitles {
				if movie.MovieBase.Subtitles == nil {
					movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Sources[0].Subtitles))
				}
				movie.MovieBase.Subtitles[s.Name] = &dbModel.Subtitle{
					URL:  s.URL,
					Type: s.Type,
				}
			}
			for _, s := range data.Sources[1:] {
				movie.MovieBase.MoreSources = append(movie.MovieBase.MoreSources,
					&dbModel.MoreSource{
						Name: s.Name,
						Url:  s.URL,
					},
				)

				for _, subt := range s.Subtitles {
					if movie.MovieBase.Subtitles == nil {
						movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(s.Subtitles))
					}
					movie.MovieBase.Subtitles[subt.Name] = &dbModel.Subtitle{
						URL:  subt.URL,
						Type: subt.Type,
					}
				}
			}
		} else {
			for si, es := range data.Sources {
				if len(es.URL) == 0 {
					if si != len(data.Sources)-1 {
						continue
					}
					if movie.MovieBase.Url == "" {
						return nil, errors.New("no source")
					}
				}

				rawPath, err := url.JoinPath("/api/movie/proxy", movie.RoomID, movie.ID)
				if err != nil {
					return nil, err
				}
				rawQuery := url.Values{}
				rawQuery.Set("source", strconv.Itoa(si))
				rawQuery.Set("token", userToken)
				u := url.URL{
					Path:     rawPath,
					RawQuery: rawQuery.Encode(),
				}
				movie.MovieBase.Url = u.String()
				movie.MovieBase.Type = utils.GetUrlExtension(es.URL)

				if len(es.Subtitles) == 0 {
					continue
				}
				for sbi, s := range es.Subtitles {
					if movie.MovieBase.Subtitles == nil {
						movie.MovieBase.Subtitles = make(map[string]*dbModel.Subtitle, len(es.Subtitles))
					}
					rawQuery := url.Values{}
					rawQuery.Set("t", "subtitle")
					rawQuery.Set("source", strconv.Itoa(si))
					rawQuery.Set("id", strconv.Itoa(sbi))
					rawQuery.Set("token", userToken)
					u := url.URL{
						Path:     rawPath,
						RawQuery: rawQuery.Encode(),
					}
					movie.MovieBase.Subtitles[s.Name] = &dbModel.Subtitle{
						URL:  u.String(),
						Type: s.Type,
					}
				}
			}
		}

		return &movie, nil

	default:
		return nil, fmt.Errorf("vendor not implement gen movie url")
	}
}
