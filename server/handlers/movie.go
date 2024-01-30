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
	"path/filepath"
	"strconv"
	"strings"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	"github.com/synctv-org/synctv/internal/settings"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
	"golang.org/x/exp/maps"
)

func GetPageItems[T any](ctx *gin.Context, items []T) ([]T, error) {
	page, max, err := utils.GetPageAndMax(ctx)
	if err != nil {
		return nil, err
	}

	return utils.GetPageItems(items, page, max), nil
}

func MovieList(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	page, max, err := utils.GetPageAndMax(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	currentResp, err := genCurrentResp(ctx, user, room)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	m := room.GetMoviesWithPage(page, max)
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

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": currentResp,
		"total":   room.GetMoviesCount(),
		"movies":  mresp,
	}))
}

func genCurrentResp(ctx context.Context, user *op.User, room *op.Room) (*model.CurrentMovieResp, error) {
	return genCurrentRespWithCurrent(ctx, user, room, room.Current())
}

func genCurrentRespWithCurrent(ctx context.Context, user *op.User, room *op.Room, current *op.Current) (*model.CurrentMovieResp, error) {
	if current.Movie.ID == "" {
		return &model.CurrentMovieResp{}, nil
	}
	opMovie, err := room.GetMovieByID(current.Movie.ID)
	if err != nil {
		return nil, err
	}
	var movie *dbModel.Movie = &opMovie.Movie
	if current.Movie.Base.VendorInfo.Vendor != "" {
		vendorMovie, err := genVendorMovie(ctx, user, opMovie)
		if err != nil {
			return nil, err
		}
		movie = vendorMovie
	} else if current.Movie.Base.RtmpSource || current.Movie.Base.Live && current.Movie.Base.Proxy {
		switch current.Movie.Base.Type {
		case "m3u8":
			current.Movie.Base.Url = fmt.Sprintf("/api/movie/live/hls/list/%s.m3u8", current.Movie.ID)
		case "flv":
			current.Movie.Base.Url = fmt.Sprintf("/api/movie/live/flv/%s.flv", current.Movie.ID)
		default:
			return nil, errors.New("not support live movie type")
		}
		current.Movie.Base.Headers = nil
	} else if current.Movie.Base.Proxy {
		current.Movie.Base.Url = fmt.Sprintf("/api/movie/proxy/%s/%s", current.Movie.RoomID, current.Movie.ID)
		current.Movie.Base.Headers = nil
	}
	if current.Movie.Base.Type == "" && current.Movie.Base.Url != "" {
		current.Movie.Base.Type = utils.GetUrlExtension(current.Movie.Base.Url)
	}
	current.UpdateSeek()
	resp := &model.CurrentMovieResp{
		Status: current.Status,
		Movie: model.MoviesResp{
			Id:        movie.ID,
			CreatedAt: movie.CreatedAt.UnixMilli(),
			Base:      movie.Base,
			Creator:   op.GetUserName(movie.CreatorID),
			CreatorId: movie.CreatorID,
		},
		ExpireId: opMovie.ExpireId(),
	}
	return resp, nil
}

func CurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	currentResp, err := genCurrentResp(ctx, user, room)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, model.NewApiDataResp(currentResp))
}

func Movies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	page, max, err := utils.GetPageAndMax(ctx)
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
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func PushMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
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

	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func DelMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ClearMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

	if err := user.ClearMovies(room); err != nil {
		if errors.Is(err, dbModel.ErrNoPermission) {
			ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorResp(err))
			return
		}
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
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

	if err := room.SwapMoviePositions(req.Id1, req.Id2); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_MOVIES_CHANGED,
		MoviesChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ChangeCurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	user := ctx.MustGet("user").(*op.UserEntry).Value()

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

	if err := room.Broadcast(&pb.ElementMessage{
		Type: pb.ElementMessageType_CURRENT_CHANGED,
		CurrentChanged: &pb.Sender{
			Username: user.Username,
			Userid:   user.ID,
		},
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

	m, err := room.Value().GetMovieByID(ctx.Param("movieId"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if m.Movie.Base.VendorInfo.Vendor != "" {
		proxyVendorMovie(ctx, m)
		return
	}

	if !m.Movie.Base.Proxy || m.Movie.Base.Live || m.Movie.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
		return
	}

	switch m.Movie.Base.Type {
	case "mpd":
		// TODO: cache mpd file
		fallthrough
	default:
		err = proxyURL(ctx, m.Movie.Base.Url, m.Movie.Base.Headers)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
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
// 		resp, err := http.DefaultClient.Do(req)
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
	req.Header.Set("Accept-Encoding", ctx.GetHeader("Accept-Encoding"))
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()
	ctx.Header("Accept-Ranges", resp.Header.Get("Accept-Ranges"))
	ctx.Header("Cache-Control", resp.Header.Get("Cache-Control"))
	ctx.Header("Content-Length", resp.Header.Get("Content-Length"))
	ctx.Header("Content-Range", resp.Header.Get("Content-Range"))
	ctx.Header("Content-Type", resp.Header.Get("Content-Type"))
	ctx.Status(resp.StatusCode)
	_, err = io.Copy(ctx.Writer, resp.Body)
	if err != nil && err != io.EOF {
		return err
	}
	return nil
}

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinLive(ctx *gin.Context) {
	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.Trim(ctx.Param("movieId"), "/")
	m, err := room.GetMovieByID(movieId)
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

	joinType := ctx.DefaultQuery("type", "auto")
	if joinType == "auto" {
		joinType = m.Movie.Base.Type
	}
	switch joinType {
	case "flv":
		w := httpflv.NewHttpFLVWriter(ctx.Writer)
		defer w.Close()
		err = channel.AddPlayer(w)
		if err != nil {
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
			return fmt.Sprintf("/api/movie/live/hls/data/%s/%s/%s.%s", room.ID, movieId, tsName, ext)
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp(fmt.Sprintf("not support join type: %s", joinType)))
		return
	}
}

func JoinFlvLive(ctx *gin.Context) {
	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".flv")
	m, err := room.GetMovieByID(movieId)
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

	w := httpflv.NewHttpFLVWriter(ctx.Writer)
	defer w.Close()
	err = channel.AddPlayer(w)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	_ = w.SendPacket()
}

func JoinHlsLive(ctx *gin.Context) {
	ctx.Header("Cache-Control", "no-store")
	room := ctx.MustGet("room").(*op.RoomEntry).Value()
	movieId := strings.TrimSuffix(strings.Trim(ctx.Param("movieId"), "/"), ".m3u8")
	m, err := room.GetMovieByID(movieId)
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

	b, err := channel.GenM3U8File(func(tsName string) (tsPath string) {
		ext := "ts"
		if settings.TsDisguisedAsPng.Get() {
			ext = "png"
		}
		return fmt.Sprintf("/api/movie/live/hls/data/%s/%s/%s.%s", room.ID, movieId, tsName, ext)
	})
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
}

func ServeHlsLive(ctx *gin.Context) {
	ctx.Header("Cache-Control", "no-store")
	roomId := ctx.Param("roomId")
	roomE, err := op.LoadOrInitRoomByID(roomId)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
		return
	}
	room := roomE.Value()
	movieId := ctx.Param("movieId")
	m, err := room.GetMovieByID(movieId)
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

	dataId := ctx.Param("dataId")
	switch fileExt := filepath.Ext(dataId); fileExt {
	case ".ts":
		if settings.TsDisguisedAsPng.Get() {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(strings.TrimSuffix(dataId, fileExt))
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
		b, err := channel.GetTsFile(strings.TrimSuffix(dataId, fileExt))
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

func proxyVendorMovie(ctx *gin.Context, movie *op.Movie) {
	switch movie.Movie.Base.VendorInfo.Vendor {
	case dbModel.VendorBilibili:
		t := ctx.Query("t")
		switch t {
		case "", "hevc":
			if !movie.Movie.Base.Proxy {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
				return
			}
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			mpdC, err := movie.BilibiliCache().SharedMpd.Get(ctx, u.Value().BilibiliCache())
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			if id := ctx.Query("id"); id == "" {
				if t == "hevc" {
					ctx.Data(http.StatusOK, "application/dash+xml", []byte(mpdC.HevcMpd))
				} else {
					ctx.Data(http.StatusOK, "application/dash+xml", []byte(mpdC.Mpd))
				}
				return
			} else {
				streamId, err := strconv.Atoi(id)
				if err != nil {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
					return
				}
				if streamId >= len(mpdC.Urls) {
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
				proxyURL(ctx, mpdC.Urls[streamId], headers)
				return
			}
		case "subtitle":
			id := ctx.Query("n")
			if id == "" {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("n is empty"))
				return
			}
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			srtI, err := movie.BilibiliCache().Subtitle.Get(ctx, u.Value().BilibiliCache())
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			if s, ok := srtI[id]; ok {
				srtData, err := s.Srt.Get(ctx)
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

	case dbModel.VendorAlist:
		u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		alistC, err := movie.AlistCache().Get(ctx, u.Value().AlistCache())
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
		if alistC.Ali != nil {
			t := ctx.Query("t")
			switch t {
			case "":
				ctx.Data(http.StatusOK, "audio/mpegurl", alistC.Ali.M3U8ListFile)
				return
			case "subtitle":
				idS := ctx.Query("id")
				if idS == "" {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id is empty"))
					return
				}
				id, err := strconv.Atoi(idS)
				if err != nil {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
					return
				}
				if id >= len(alistC.Ali.Subtitles) {
					ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
					return
				}
				data, err := alistC.Ali.Subtitles[id].Cache.Get(ctx)
				if err != nil {
					ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
					return
				}
				ctx.Data(http.StatusOK, "text/plain; charset=utf-8", data)
			}
		} else if !movie.Movie.Base.Proxy {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
			return
		} else {
			proxyURL(ctx, alistC.URL, nil)
		}

		return

	case dbModel.VendorEmby:
		t := ctx.Query("t")
		switch t {
		case "":
			if !movie.Movie.Base.Proxy {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
				return
			}
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			embyC, err := movie.EmbyCache().Get(ctx, u.Value().EmbyCache())
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			source, err := strconv.Atoi(ctx.Query("source"))
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if source >= len(embyC.Sources) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("source out of range"))
				return
			}
			id, err := strconv.Atoi(ctx.Query("id"))
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if id >= len(embyC.Sources[source].URLs) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
				return
			}
			proxyURL(ctx, embyC.Sources[source].URLs[id].URL, nil)
			return

		case "subtitle":
			u, err := op.LoadOrInitUserByID(movie.Movie.CreatorID)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			embyC, err := movie.EmbyCache().Get(ctx, u.Value().EmbyCache())
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			source, err := strconv.Atoi(ctx.Query("source"))
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if source >= len(embyC.Sources) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("source out of range"))
				return
			}
			id, err := strconv.Atoi(ctx.Query("id"))
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
				return
			}
			if id >= len(embyC.Sources[source].Subtitles) {
				ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("id out of range"))
				return
			}
			data, err := embyC.Sources[source].Subtitles[id].Cache.Get(ctx)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
				return
			}
			ctx.Data(http.StatusOK, "text/plain; charset=utf-8", data)
			return
		}

	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("vendor not support proxy"))
		return
	}
}

// user is the api requester
func genVendorMovie(ctx context.Context, user *op.User, opMovie *op.Movie) (*dbModel.Movie, error) {
	movie := opMovie.Movie
	var err error
	switch movie.Base.VendorInfo.Vendor {
	case dbModel.VendorBilibili:
		bmc := opMovie.BilibiliCache()
		if !movie.Base.Proxy {
			var s string
			if movie.Base.VendorInfo.Bilibili.Shared {
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

			movie.Base.Url = s
		} else {
			movie.Base.Url = fmt.Sprintf("/api/movie/proxy/%s/%s", movie.RoomID, movie.ID)
			movie.Base.Type = "mpd"
		}
		srt, err := bmc.Subtitle.Get(ctx, user.BilibiliCache())
		if err != nil {
			return nil, err
		}
		for k := range srt {
			if movie.Base.Subtitles == nil {
				movie.Base.Subtitles = make(map[string]*dbModel.Subtitle, len(srt))
			}
			movie.Base.Subtitles[k] = &dbModel.Subtitle{
				URL:  fmt.Sprintf("/api/movie/proxy/%s/%s?t=subtitle&n=%s", movie.RoomID, movie.ID, k),
				Type: "srt",
			}
		}
		return &movie, nil

	case dbModel.VendorAlist:
		creator, err := op.LoadOrInitUserByID(movie.CreatorID)
		if err != nil {
			return nil, err
		}
		data, err := opMovie.AlistCache().Get(ctx, creator.Value().AlistCache())
		if err != nil {
			return nil, err
		}

		if len(data.Ali.M3U8ListFile) != 0 {
			rawPath, err := url.JoinPath("/api/movie/proxy", movie.RoomID, movie.ID)
			if err != nil {
				return nil, err
			}
			u := url.URL{
				Path: rawPath,
			}
			movie.Base.Url = u.String()
			movie.Base.Type = "m3u8"

			for i, s := range data.Ali.Subtitles {
				if movie.Base.Subtitles == nil {
					movie.Base.Subtitles = make(map[string]*dbModel.Subtitle, len(data.Ali.Subtitles))
				}
				movie.Base.Subtitles[s.Raw.Language] = &dbModel.Subtitle{
					URL:  fmt.Sprintf("/api/movie/proxy/%s/%s?t=subtitle&id=%d", movie.RoomID, movie.ID, i),
					Type: utils.GetUrlExtension(s.Raw.Url),
				}
			}
		} else if !movie.Base.Proxy {
			movie.Base.Url = data.URL
		} else {
			rawPath, err := url.JoinPath("/api/movie/proxy", movie.RoomID, movie.ID)
			if err != nil {
				return nil, err
			}
			u := url.URL{
				Path: rawPath,
			}
			movie.Base.Url = u.String()
			movie.Base.Type = utils.GetUrlExtension(data.URL)
		}
		movie.Base.VendorInfo.Alist.Password = ""
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

		if !movie.Base.Proxy {
			for i, es := range data.Sources {
				if len(es.URLs) == 0 {
					if i != len(data.Sources)-1 {
						continue
					}
					if movie.Base.Url == "" {
						return nil, errors.New("no source")
					}
				}
				movie.Base.Url = es.URLs[0].URL

				if len(es.Subtitles) == 0 {
					continue
				}
				for _, s := range es.Subtitles {
					if movie.Base.Subtitles == nil {
						movie.Base.Subtitles = make(map[string]*dbModel.Subtitle, len(es.Subtitles))
					}
					movie.Base.Subtitles[s.Name] = &dbModel.Subtitle{
						URL:  s.URL,
						Type: s.Type,
					}
				}
			}
		} else {
			for si, es := range data.Sources {
				if len(es.URLs) == 0 {
					if si != len(data.Sources)-1 {
						continue
					}
					if movie.Base.Url == "" {
						return nil, errors.New("no source")
					}
				}

				rawPath, err := url.JoinPath("/api/movie/proxy", movie.RoomID, movie.ID)
				if err != nil {
					return nil, err
				}
				rawQuery := url.Values{}
				rawQuery.Set("source", strconv.Itoa(si))
				rawQuery.Set("id", strconv.Itoa(0))
				u := url.URL{
					Path:     rawPath,
					RawQuery: rawQuery.Encode(),
				}
				movie.Base.Url = u.String()
				movie.Base.Type = utils.GetUrlExtension(es.URLs[0].URL)

				if len(es.Subtitles) == 0 {
					continue
				}
				for sbi, s := range es.Subtitles {
					if movie.Base.Subtitles == nil {
						movie.Base.Subtitles = make(map[string]*dbModel.Subtitle, len(es.Subtitles))
					}
					rawQuery := url.Values{}
					rawQuery.Set("t", "subtitle")
					rawQuery.Set("source", strconv.Itoa(si))
					rawQuery.Set("id", strconv.Itoa(sbi))
					u := url.URL{
						Path:     rawPath,
						RawQuery: rawQuery.Encode(),
					}
					movie.Base.Subtitles[s.Name] = &dbModel.Subtitle{
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
