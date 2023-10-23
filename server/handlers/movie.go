package handlers

import (
	"errors"
	"fmt"
	"net/http"
	"path"
	"path/filepath"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/go-resty/resty/v2"
	"github.com/synctv-org/synctv/internal/conf"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	pb "github.com/synctv-org/synctv/proto"
	"github.com/synctv-org/synctv/proxy"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
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
	// user := ctx.MustGet("user").(*op.User)

	page, max, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m := room.GetMoviesWithPage(page, max)

	mresp := make([]model.MoviesResp, len(m))
	for i, v := range m {
		mresp[i] = model.MoviesResp{
			Id:      v.ID,
			Base:    m[i].Base,
			PullKey: v.PullKey,
			Creater: op.GetUserName(v.CreatorID),
		}
	}

	current := room.Current()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": &model.CurrentMovieResp{
			Status: current.Status,
			Movie: model.MoviesResp{
				Id:      current.Movie.ID,
				Base:    current.Movie.Base,
				PullKey: current.Movie.PullKey,
				Creater: op.GetUserName(current.Movie.CreatorID),
			},
		},
		"total":  room.GetMoviesCount(),
		"movies": mresp,
	}))
}

func CurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	// user := ctx.MustGet("user").(*op.User)

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": room.Current(),
	}))
}

func Movies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	// user := ctx.MustGet("user").(*op.User)

	page, max, err := GetPageAndPageSize(ctx)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m := room.GetMoviesWithPage(int(page), int(max))

	mresp := make([]model.MoviesResp, len(m))
	for i, v := range m {
		mresp[i] = model.MoviesResp{
			Id:      v.ID,
			Base:    m[i].Base,
			PullKey: v.PullKey,
			Creater: op.GetUserName(v.CreatorID),
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

	mi := user.NewMovie(dbModel.MovieInfo{
		Base: dbModel.BaseMovieInfo(req),
	})

	err := room.AddMovie(mi)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		ElementMessage: &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_MOVIES,
			Sender: user.Username,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func NewPublishKey(ctx *gin.Context) {
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

	if !user.HasPermission(room.ID, dbModel.CanCreateUserPublishKey) && movie.CreatorID != user.ID {
		ctx.AbortWithStatus(http.StatusForbidden)
		return
	}

	if !movie.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("only live movie can get publish key"))
		return
	}

	if movie.PullKey == "" {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorStringResp("pull key is empty"))
		return
	}

	token, err := rtmp.NewRtmpAuthorization(movie.PullKey)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	host := conf.Conf.Rtmp.CustomPublishHost
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

	if err := room.UpdateMovie(req.Id, dbModel.BaseMovieInfo(req.PushMovieReq)); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		ElementMessage: &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_MOVIES,
			Sender: user.Username,
		},
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

	for _, id := range req.Ids {
		err := room.DeleteMovieByID(id)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}

	if err := room.Broadcast(&op.ElementMessage{
		ElementMessage: &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_MOVIES,
			Sender: user.Username,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ClearMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	if err := room.ClearMovies(); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.Broadcast(&op.ElementMessage{
		ElementMessage: &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_MOVIES,
			Sender: user.Username,
		},
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
		ElementMessage: &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_MOVIES,
			Sender: user.Username,
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ChangeCurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.IdReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if err := room.ChangeCurrentMovie(req.Id); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}
	if err := room.Broadcast(&op.ElementMessage{
		ElementMessage: &pb.ElementMessage{
			Type:    pb.ElementMessageType_CHANGE_CURRENT,
			Sender:  user.Username,
			Current: room.Current().Proto(),
		},
	}); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

var allowedProxyMovieContentType = map[string]struct{}{
	"video/avi":  {},
	"video/mp4":  {},
	"video/webm": {},
}

const UserAgent = `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36 Edg/117.0.2045.40`

func ProxyMovie(ctx *gin.Context) {
	roomId := ctx.Param("roomId")
	if roomId == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("roomId is empty"))
		return
	}
	id, err := strconv.ParseUint(roomId, 10, 64)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	room, err := op.LoadOrInitRoomByID(uint(id))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	m, err := room.GetMovieWithPullKey(ctx.Param("pullKey"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	if !m.Base.Proxy || m.Base.Live || m.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support proxy"))
		return
	}

	if l, err := utils.ParseURLIsLocalIP(m.Base.Url); err != nil || l {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("parse url error or url is local ip"))
		return
	}

	r := resty.New().R()

	for k, v := range m.Base.Headers {
		r.SetHeader(k, v)
	}
	resp, err := r.Head(m.Base.Url)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	defer resp.RawBody().Close()

	if _, ok := allowedProxyMovieContentType[resp.Header().Get("Content-Type")]; !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(fmt.Errorf("this movie type support proxy: %s", resp.Header().Get("Content-Type"))))
		return
	}
	ctx.Status(resp.StatusCode())
	ctx.Header("Content-Type", resp.Header().Get("Content-Type"))
	l := resp.Header().Get("Content-Length")
	ctx.Header("Content-Length", l)
	ctx.Header("Content-Encoding", resp.Header().Get("Content-Encoding"))

	length, err := strconv.ParseInt(l, 10, 64)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	hrs := proxy.NewBufferedHttpReadSeeker(128*1024, m.Base.Url,
		proxy.WithContext(ctx),
		proxy.WithHeaders(m.Base.Headers),
		proxy.WithContext(ctx),
		proxy.WithContentLength(length),
	)
	name := resp.Header().Get("Content-Disposition")
	if name == "" {
		name = filepath.Base(resp.Request.RawRequest.URL.Path)
	} else {
		ctx.Header("Content-Disposition", name)
	}
	http.ServeContent(ctx.Writer, ctx.Request, name, time.Now(), hrs)
}

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinLive(ctx *gin.Context) {
	if !conf.Conf.Proxy.LiveProxy && !conf.Conf.Rtmp.Enable {
		ctx.AbortWithStatusJSON(http.StatusForbidden, model.NewApiErrorStringResp("live proxy and rtmp source is not enabled"))
		return
	}
	room := ctx.MustGet("room").(*op.Room)
	// user := ctx.MustGet("user").(*op.User)

	pullKey := strings.Trim(ctx.Param("pullKey"), "/")
	pullKeySplitd := strings.Split(pullKey, "/")
	fileName := pullKeySplitd[0]
	fileExt := path.Ext(pullKey)
	channelName := strings.TrimSuffix(fileName, fileExt)
	// m, err := room.GetMovieWithPullKey(channelName)
	// if err != nil {
	// 	ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
	// 	return
	// }
	channel, err := room.GetChannel(channelName)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
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
		b, err := channel.GenM3U8PlayList(fmt.Sprintf("/api/movie/live/%s", channelName))
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b.Bytes())
	case ".ts":
		b, err := channel.GetTsFile(pullKeySplitd[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Data(http.StatusOK, hls.TSContentType, b)
	default:
		ctx.Header("Cache-Control", "no-store")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
	}
}
