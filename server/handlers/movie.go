package handlers

import (
	"errors"
	"fmt"
	"image"
	"image/color"
	"image/png"
	"math/rand"
	"net/http"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/db"
	dbModel "github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/op"
	"github.com/synctv-org/synctv/internal/rtmp"
	pb "github.com/synctv-org/synctv/proto/message"
	"github.com/synctv-org/synctv/proxy"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/synctv/vendors/bilibili"
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
	user := ctx.MustGet("user").(*op.User)

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
			Creator: op.GetUserName(v.CreatorID),
		}
		// hide headers when proxy
		if mresp[i].Base.Proxy {
			mresp[i].Base.Headers = nil
		}
	}

	current, err := genCurrent(room.Current(), user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	current.UpdateSeek()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": genCurrentResp(current),
		"total":   room.GetMoviesCount(),
		"movies":  mresp,
	}))
}

func genCurrent(current *op.Current, userID string) (*op.Current, error) {
	if current.Movie.Base.VendorInfo.Vendor != "" {
		return current, parse2VendorMovie(userID, &current.Movie, !current.Movie.Base.Proxy)
	}
	return current, nil
}

func genCurrentResp(current *op.Current) *model.CurrentMovieResp {
	c := &model.CurrentMovieResp{
		Status: current.Status,
		Movie: model.MoviesResp{
			Id:      current.Movie.ID,
			Base:    current.Movie.Base,
			Creator: op.GetUserName(current.Movie.CreatorID),
		},
	}
	// hide headers when proxy
	if c.Movie.Base.Proxy {
		c.Movie.Base.Headers = nil
	}
	return c
}

func CurrentMovie(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	current, err := genCurrent(room.Current(), user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}

	current.UpdateSeek()

	ctx.JSON(http.StatusOK, model.NewApiDataResp(gin.H{
		"current": genCurrentResp(current),
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
			Creator: op.GetUserName(v.CreatorID),
		}
		// hide headers when proxy
		if mresp[i].Base.Proxy {
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

	mi := user.NewMovie((*dbModel.BaseMovie)(&req))

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

func PushMovies(ctx *gin.Context) {
	room := ctx.MustGet("room").(*op.Room)
	user := ctx.MustGet("user").(*op.User)

	req := model.PushMoviesReq{}
	if err := model.Decode(ctx, &req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
		return
	}

	var ms []*dbModel.Movie = make([]*dbModel.Movie, len(req))

	for i, v := range req {
		m := (*dbModel.BaseMovie)(v)
		err := m.Validate()
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
		ms[i] = user.NewMovie(m)
	}

	for _, m := range ms {
		err := room.AddMovie(m)
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

	token, err := rtmp.NewRtmpAuthorization(movie.ID)
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

	if err := room.UpdateMovie(req.Id, dbModel.BaseMovie(req.PushMovieReq)); err != nil {
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

	current, err := genCurrent(room.Current(), user.ID)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
		return
	}
	current.UpdateSeek()

	if (current.Movie.Base.VendorInfo.Vendor == "") || (current.Movie.Base.VendorInfo.Vendor != "" && current.Movie.Base.VendorInfo.Shared) {
		if err := room.Broadcast(&op.ElementMessage{
			ElementMessage: &pb.ElementMessage{
				Type:    pb.ElementMessageType_CHANGE_CURRENT,
				Sender:  user.Username,
				Current: current.Proto(),
			},
		}); err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
	} else {
		if err := room.SendToUser(user, &op.ElementMessage{
			ElementMessage: &pb.ElementMessage{
				Type:    pb.ElementMessageType_CHANGE_CURRENT,
				Sender:  user.Username,
				Current: current.Proto(),
			},
		}); err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}

		m := &pb.ElementMessage{
			Type:   pb.ElementMessageType_CHANGE_CURRENT,
			Sender: user.Username,
		}
		if err := room.Broadcast(&op.ElementMessage{
			ElementMessage: m,
			BeforeSendFunc: func(sendTo *op.User) error {
				current, err := genCurrent(room.Current(), sendTo.ID)
				if err != nil {
					return err
				}
				current.UpdateSeek()
				m.Current = current.Proto()
				return nil
			},
		}, op.WithIgnoreId(user.ID)); err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, model.NewApiErrorResp(err))
			return
		}
	}

	ctx.Status(http.StatusNoContent)
}

func ProxyMovie(ctx *gin.Context) {
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

	if !m.Base.Proxy || m.Base.Live || m.Base.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("not support movie proxy"))
		return
	}

	if m.Base.VendorInfo.Vendor != "" {
		err = parse2VendorMovie(m.Movie.CreatorID, m.Movie, true)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(err))
			return
		}
	}

	if l, err := utils.ParseURLIsLocalIP(m.Base.Url); err != nil || l {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorStringResp("parse url error or url is local ip"))
		return
	}

	hrs := proxy.NewBufferedHttpReadSeeker(256*1024, m.Base.Url,
		proxy.WithContext(ctx),
		proxy.WithHeaders(m.Base.Headers),
	)
	http.ServeContent(ctx.Writer, ctx.Request, m.Base.Url, time.Now(), hrs)
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

	movieId := strings.Trim(ctx.Param("movieId"), "/")
	movieIdSplitd := strings.Split(movieId, "/")
	fileName := movieIdSplitd[0]
	fileExt := path.Ext(movieId)
	channelName := strings.TrimSuffix(fileName, fileExt)
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
		b, err := channel.GenM3U8File(func(tsName string) (tsPath string) {
			ext := "ts"
			if conf.Conf.Rtmp.TsDisguisedAsPng {
				ext = "png"
			}
			return fmt.Sprintf("/api/movie/live/%s.%s", channelName, ext)
		})
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b)
	case ".ts":
		if conf.Conf.Rtmp.TsDisguisedAsPng {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(movieIdSplitd[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Data(http.StatusOK, hls.TSContentType, b)
	case ".png":
		if !conf.Conf.Rtmp.TsDisguisedAsPng {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
			return
		}
		b, err := channel.GetTsFile(movieIdSplitd[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, model.NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Header("Content-Type", "image/png")
		img := image.NewGray(image.Rect(0, 0, 1, 1))
		img.Set(1, 1, color.Gray{uint8(rand.Intn(255))})
		png.Encode(ctx.Writer, img)
		ctx.Writer.Write(b)
	default:
		ctx.Header("Cache-Control", "no-store")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, model.NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
	}
}

func parse2VendorMovie(userID string, movie *dbModel.Movie, getUrl bool) (err error) {
	if movie.Base.VendorInfo.Shared {
		userID = movie.CreatorID
	}

	switch movie.Base.VendorInfo.Vendor {
	case dbModel.StreamingVendorBilibili:
		info := movie.Base.VendorInfo.Bilibili

		vendor, err := db.AssignFirstOrCreateVendorByUserIDAndVendor(userID, dbModel.StreamingVendorBilibili)
		if err != nil {
			return err
		}
		cli, err := bilibili.NewClient(vendor.Cookies)
		if err != nil {
			return err
		}

		if getUrl {
			var mu *bilibili.VideoURL
			if info.Bvid != "" {
				mu, err = cli.GetVideoURL(0, info.Bvid, info.Cid, bilibili.WithQuality(info.Quality))
			} else if info.Epid != 0 {
				mu, err = cli.GetPGCURL(info.Epid, 0, bilibili.WithQuality(info.Quality))
			} else {
				err = errors.New("bvid and epid are empty")
			}
			if err != nil {
				return err
			}
			movie.Base.Url = mu.URL
		}

		return nil

	default:
		return fmt.Errorf("vendor not support")
	}
}
