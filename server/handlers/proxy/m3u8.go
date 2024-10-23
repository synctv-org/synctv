package proxy

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/synctv/utils/m3u8"
	"github.com/zijiren233/go-uhc"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/stream"
)

type m3u8TargetClaims struct {
	RoomId    string `json:"r"`
	MovieId   string `json:"m"`
	TargetUrl string `json:"t"`
	jwt.RegisteredClaims
}

func GetM3u8Target(token string) (*m3u8TargetClaims, error) {
	t, err := jwt.ParseWithClaims(token, &m3u8TargetClaims{}, func(token *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil || !t.Valid {
		return nil, errors.New("auth failed")
	}
	claims, ok := t.Claims.(*m3u8TargetClaims)
	if !ok {
		return nil, errors.New("auth failed")
	}
	return claims, nil
}

func NewM3u8TargetToken(targetUrl, roomId, movieId string) (string, error) {
	claims := &m3u8TargetClaims{
		RoomId:    roomId,
		MovieId:   movieId,
		TargetUrl: targetUrl,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

func ProxyM3u8(ctx *gin.Context, u string, headers map[string]string, isM3u8File bool, token, roomId, movieId string) error {
	if !isM3u8File {
		return ProxyURL(ctx, u, headers)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		return fmt.Errorf("new request error: %w", err)
	}
	for k, v := range headers {
		req.Header.Set(k, v)
	}
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	resp, err := uhc.Do(req)
	if err != nil {
		return fmt.Errorf("do request error: %w", err)
	}
	defer resp.Body.Close()
	b, err := io.ReadAll(resp.Body)
	if err != nil {
		return fmt.Errorf("read response body error: %w", err)
	}
	m3u8Str, err := m3u8.ReplaceM3u8SegmentsWithBaseUrl(stream.BytesToString(b), u, func(segmentUrl string) (string, error) {
		targetToken, err := NewM3u8TargetToken(segmentUrl, roomId, movieId)
		if err != nil {
			return "", err
		}
		return fmt.Sprintf("/api/room/movie/proxy/%s/m3u8/%s?token=%s&roomId=%s", movieId, targetToken, token, roomId), nil
	})
	if err != nil {
		return fmt.Errorf("replace m3u8 segments with base url error: %w", err)
	}
	ctx.Data(http.StatusOK, hls.M3U8ContentType, stream.StringToBytes(m3u8Str))
	return nil
}
