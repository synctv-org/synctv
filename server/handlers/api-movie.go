package handlers

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"net/url"
	"path"
	"strconv"
	"strings"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/go-resty/resty/v2"
	"github.com/golang-jwt/jwt/v5"
	"github.com/google/uuid"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/room"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/livelib/av"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/livelib/protocol/httpflv"
	"github.com/zijiren233/livelib/protocol/rtmp"
	"github.com/zijiren233/livelib/protocol/rtmp/core"
	rtmps "github.com/zijiren233/livelib/server"
	"github.com/zijiren233/stream"
)

func GetPageItems[T any](ctx *gin.Context, items []T) ([]T, error) {
	max, err := strconv.ParseUint(ctx.DefaultQuery("max", "10"), 10, 64)
	if err != nil {
		return items, errors.New("max must be a number")
	}

	page, err := strconv.ParseUint(ctx.DefaultQuery("page", "1"), 10, 64)
	if err != nil {
		return items, errors.New("page must be a number")
	}

	return utils.GetPageItems(items, int(max), int(page)), nil
}

func MovieList(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	ml := user.MovieList()

	movies, err := GetPageItems(ctx, ml)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"current": user.Room().Current(),
		"total":   len(ml),
		"movies":  movies,
	}))
}

func CurrentMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"current": user.Room().Current(),
	}))
}

func Movies(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	ml := user.MovieList()

	movies, err := GetPageItems(ctx, ml)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"total":  len(ml),
		"movies": movies,
	}))
}

type PushMovieReq = room.BaseMovie

func PushMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(PushMovieReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	movie, err := user.NewMovieWithBaseMovie(*req)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	switch {
	case movie.RtmpSource && movie.Proxy:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("rtmp source and proxy can not be true at the same time"))
		return
	case movie.Live && movie.RtmpSource:
		if !conf.Conf.Rtmp.Enable {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("rtmp source is not enabled"))
			return
		}
		movie.PullKey = uuid.New().String()
		c, err := user.Room().NewLiveChannel(movie.PullKey)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
			return
		}
		movie.SetChannel(c)
	case movie.Live && movie.Proxy:
		if !conf.Conf.Proxy.LiveProxy {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("live proxy is not enabled"))
			return
		}
		u, err := url.Parse(movie.Url)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
			return
		}
		switch u.Scheme {
		case "rtmp":
			PullKey := uuid.New().String()
			c, err := user.Room().NewLiveChannel(PullKey)
			if err != nil {
				ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
				return
			}
			movie.PullKey = PullKey
			go func() {
				for {
					cli := core.NewConnClient()
					err = cli.Start(movie.PullKey, av.PLAY)
					if err != nil {
						cli.Close()
						time.Sleep(time.Second)
						continue
					}
					if err := c.PushStart(rtmp.NewReader(cli)); err != nil && err == rtmps.ErrClosed {
						cli.Close()
						time.Sleep(time.Second)
						continue
					}
					return
				}
			}()
		case "http", "https":
			// TODO: http https flv proxy
			fallthrough
		default:
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("only support rtmp temporarily"))
			return
		}
	case !movie.Live && movie.RtmpSource:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("rtmp source must be live"))
		return
	case !movie.Live && movie.Proxy:
		if !conf.Conf.Proxy.MovieProxy {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("movie proxy is not enabled"))
			return
		}
		fallthrough
	case !movie.Live && !movie.Proxy, movie.Live && !movie.Proxy && !movie.RtmpSource:
		u, err := url.Parse(movie.Url)
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
			return
		}
		if u.Scheme != "http" && u.Scheme != "https" {
			ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("only support http or https"))
			return
		}
	default:
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("unknown error"))
		return
	}

	s := ctx.DefaultQuery("pos", "back")
	switch s {
	case "back":
		err = user.Room().PushBackMovie(movie)
	case "front":
		err = user.Room().PushFrontMovie(movie)
	default:
		err = FormatErrNotSupportPosition(s)
	}
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Broadcast(&room.ElementMessage{
		Type:   room.ChangeMovieList,
		Sender: user.Name(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.JSON(http.StatusCreated, NewApiDataResp(gin.H{
		"id": movie.Id(),
	}))
}

func NewPublishKey(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatus(http.StatusUnauthorized)
		return
	}

	req := new(IdReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	movie, err := user.Room().GetMovie(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if movie.Creator().Name() != user.Name() {
		ctx.AbortWithStatus(http.StatusForbidden)
		return
	}

	if !movie.RtmpSource {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("only live movie can get publish key"))
		return
	}

	if movie.PullKey == "" {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorStringResp("pull key is empty"))
		return
	}

	token, err := NewRtmpAuthorization(movie.PullKey)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	host := conf.Conf.Rtmp.CustomPublishHost
	if host == "" {
		host = ctx.Request.Host
	}

	ctx.JSON(http.StatusOK, NewApiDataResp(gin.H{
		"host":  host,
		"app":   user.Room().ID(),
		"token": token,
	}))
}

type EditMovieReq struct {
	Id      uint64              `json:"id"`
	Url     string              `json:"url"`
	Name    string              `json:"name"`
	Type    string              `json:"type"`
	Headers map[string][]string `json:"headers"`
}

func EditMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(EditMovieReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	m, err := user.Room().GetMovie(req.Id)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	// Dont edit live and proxy

	m.Url = req.Url
	m.Name = req.Name
	m.Type = req.Type
	m.Headers = req.Headers

	if err := user.Broadcast(&room.ElementMessage{
		Type:   room.ChangeMovieList,
		Sender: user.Name(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type IdsReq struct {
	Ids []uint64 `json:"ids"`
}

func DelMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(IdsReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Room().DelMovie(req.Ids...); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Broadcast(&room.ElementMessage{
		Type:   room.ChangeMovieList,
		Sender: user.Name(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

func ClearMovies(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	if err := user.Room().ClearMovies(); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Broadcast(&room.ElementMessage{
		Type:   room.ChangeMovieList,
		Sender: user.Name(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type SwapMovieReq struct {
	Id1 uint64 `json:"id1"`
	Id2 uint64 `json:"id2"`
}

func SwapMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(SwapMovieReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Room().SwapMovie(req.Id1, req.Id2); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Broadcast(&room.ElementMessage{
		Type:   room.ChangeMovieList,
		Sender: user.Name(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type IdReq struct {
	Id uint64 `json:"id"`
}

func ChangeCurrentMovie(ctx *gin.Context) {
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	req := new(IdReq)
	if err := json.NewDecoder(ctx.Request.Body).Decode(req); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Room().ChangeCurrentMovie(req.Id); err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	if err := user.Broadcast(&room.ElementMessage{
		Type:    room.ChangeCurrent,
		Sender:  user.Name(),
		Current: user.Room().Current(),
	}, room.WithSendToSelf()); err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	ctx.Status(http.StatusNoContent)
}

type RtmpClaims struct {
	PullKey string `json:"p"`
	jwt.RegisteredClaims
}

func AuthRtmpPublish(Authorization string) (channelName string, err error) {
	t, err := jwt.ParseWithClaims(strings.TrimPrefix(Authorization, `Bearer `), &RtmpClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil {
		return "", ErrAuthFailed
	}
	claims, ok := t.Claims.(*RtmpClaims)
	if !ok {
		return "", ErrAuthFailed
	}
	return claims.PullKey, nil
}

var allowedProxyMovieType = map[string]struct{}{
	"video/avi":  {},
	"video/mp4":  {},
	"video/webm": {},
}

func NewRtmpAuthorization(channelName string) (string, error) {
	claims := &RtmpClaims{
		PullKey: channelName,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

const UserAgent = `Mozilla/5.0 (Macintosh; Intel Mac OS X 10_15_7) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/117.0.0.0 Safari/537.36 Edg/117.0.2045.40`

func ProxyMovie(ctx *gin.Context) {
	roomid := ctx.Query("roomid")
	if roomid == "" {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("roomid is empty"))
		return
	}
	room, err := Rooms.GetRoom(roomid)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
	}
	cm := room.Current().Movie()
	if !cm.Proxy || cm.Live {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorStringResp("not support proxy"))
		return
	}

	u, err := url.Parse(cm.Url)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(err))
		return
	}

	req := resty.New().R().
		SetHeader("Range", ctx.GetHeader("Range")).
		SetHeader("User-Agent", UserAgent).
		SetHeader("Referer", fmt.Sprintf("%s://%s/", u.Scheme, u.Host)).
		SetHeader("Origin", fmt.Sprintf("%s://%s", u.Scheme, u.Host)).
		SetHeader("Accept", ctx.GetHeader("Accept")).
		SetHeader("Accept-Encoding", ctx.GetHeader("Accept-Encoding")).
		SetHeader("Accept-Language", ctx.GetHeader("Accept-Language"))

	if cm.Headers != nil {
		for k, v := range cm.Headers {
			req.SetHeader(k, v[0])
		}
	}

	resp, err := req.Get(cm.Url)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusInternalServerError, NewApiErrorResp(err))
		return
	}

	defer resp.RawBody().Close()
	if _, ok := allowedProxyMovieType[resp.Header().Get("Content-Type")]; !ok {
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(fmt.Errorf("this movie type support proxy: %s", resp.Header().Get("Content-Type"))))
		return
	}
	for k, v := range resp.Header() {
		ctx.Header(k, v[0])
	}
	ctx.Status(resp.StatusCode())
	io.Copy(ctx.Writer, resp.RawBody())
}

type FormatErrNotSupportFileType string

func (e FormatErrNotSupportFileType) Error() string {
	return fmt.Sprintf("not support file type %s", string(e))
}

func JoinLive(ctx *gin.Context) {
	if !conf.Conf.Proxy.LiveProxy && !conf.Conf.Rtmp.Enable {
		ctx.AbortWithStatusJSON(http.StatusForbidden, NewApiErrorStringResp("live proxy and rtmp source is not enabled"))
		return
	}
	user, err := AuthRoom(ctx.GetHeader("Authorization"))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusUnauthorized, NewApiErrorResp(err))
		return
	}

	pullKey := strings.Trim(ctx.Param("pullKey"), "/")
	pullKeySplitd := strings.Split(pullKey, "/")
	fileName := pullKeySplitd[0]
	fileExt := path.Ext(pullKey)
	channelName := strings.TrimSuffix(fileName, fileExt)
	m, err := user.Room().GetMovieWithPullKey(channelName)
	// channel, err := s.GetChannelWithApp(r.ID(), channelName)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
		return
	}
	channel := m.Channel()
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
			ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
			return
		}
		ctx.Data(http.StatusOK, hls.M3U8ContentType, b.Bytes())
	case ".ts":
		b, err := channel.GetTsFile(pullKeySplitd[1])
		if err != nil {
			ctx.AbortWithStatusJSON(http.StatusNotFound, NewApiErrorResp(err))
			return
		}
		ctx.Header("Cache-Control", "public, max-age=90")
		ctx.Data(http.StatusOK, hls.TSContentType, b)
	default:
		ctx.Header("Cache-Control", "no-store")
		ctx.AbortWithStatusJSON(http.StatusBadRequest, NewApiErrorResp(FormatErrNotSupportFileType(fileExt)))
	}
}
