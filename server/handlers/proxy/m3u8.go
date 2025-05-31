package proxy

import (
	"errors"
	"fmt"
	"io"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	"github.com/golang-jwt/jwt/v5"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/server/model"
	"github.com/synctv-org/synctv/utils"
	"github.com/synctv-org/synctv/utils/m3u8"
	"github.com/zijiren233/go-uhc"
	"github.com/zijiren233/livelib/protocol/hls"
	"github.com/zijiren233/stream"
)

type M3u8TargetClaims struct {
	jwt.RegisteredClaims
	RoomID     string `json:"r"`
	MovieID    string `json:"m"`
	TargetURL  string `json:"t"`
	IsM3u8File bool   `json:"f"`
}

func GetM3u8Target(token string) (*M3u8TargetClaims, error) {
	t, err := jwt.ParseWithClaims(token, &M3u8TargetClaims{}, func(_ *jwt.Token) (any, error) {
		return stream.StringToBytes(conf.Conf.Jwt.Secret), nil
	})
	if err != nil || !t.Valid {
		return nil, errors.New("auth failed")
	}
	claims, ok := t.Claims.(*M3u8TargetClaims)
	if !ok {
		return nil, errors.New("auth failed")
	}
	return claims, nil
}

func NewM3u8TargetToken(targetURL, roomID, movieID string, isM3u8File bool) (string, error) {
	claims := &M3u8TargetClaims{
		RoomID:     roomID,
		MovieID:    movieID,
		TargetURL:  targetURL,
		IsM3u8File: isM3u8File,
		RegisteredClaims: jwt.RegisteredClaims{
			NotBefore: jwt.NewNumericDate(time.Now()),
		},
	}
	return jwt.NewWithClaims(jwt.SigningMethodHS256, claims).
		SignedString(stream.StringToBytes(conf.Conf.Jwt.Secret))
}

const maxM3u8FileSize = 3 * 1024 * 1024 //

func M3u8Data(ctx *gin.Context, data []byte, baseURL, token, roomID, movieID string) error {
	hasM3u8File := false
	err := m3u8.RangeM3u8SegmentsWithBaseURL(
		stream.BytesToString(data),
		baseURL,
		func(segmentUrl string) (bool, error) {
			if utils.IsM3u8Url(segmentUrl) {
				hasM3u8File = true
				return false, nil
			}
			return true, nil
		},
	)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf("range m3u8 segments with base url error: %v", err),
			),
		)
		return fmt.Errorf("range m3u8 segments with base url error: %w", err)
	}
	m3u8Str, err := m3u8.ReplaceM3u8SegmentsWithBaseURL(
		stream.BytesToString(data),
		baseURL,
		func(segmentUrl string) (string, error) {
			targetToken, err := NewM3u8TargetToken(segmentUrl, roomID, movieID, hasM3u8File)
			if err != nil {
				return "", err
			}
			return fmt.Sprintf(
				"/api/room/movie/proxy/%s/m3u8/%s?token=%s&roomId=%s",
				movieID,
				targetToken,
				token,
				roomID,
			), nil
		},
	)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf("replace m3u8 segments with base url error: %v", err),
			),
		)
		return fmt.Errorf("replace m3u8 segments with base url error: %w", err)
	}
	ctx.Data(http.StatusOK, hls.M3U8ContentType, stream.StringToBytes(m3u8Str))
	return nil
}

// only cache non-m3u8 files
func M3u8(
	ctx *gin.Context,
	u string,
	headers map[string]string,
	isM3u8File bool,
	token, roomID, movieID string,
	opts ...Option,
) error {
	if !isM3u8File {
		return URL(ctx, u, headers, opts...)
	}
	if flags.Global.Dev {
		ctx.Header(proxyURLHeader, u)
	}

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u, nil)
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf("new request error: %v", err),
			),
		)
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
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf("do request error: %v", err),
			),
		)
		return fmt.Errorf("do request error: %w", err)
	}
	defer resp.Body.Close()
	// if contentType := resp.Header.Get("Content-Type"); !strings.HasPrefix(contentType,
	// "application/vnd.apple.mpegurl") {
	// 	return fmt.Errorf("m3u8 file is not a valid m3u8 file, content type: %s", contentType)
	// }
	if resp.ContentLength > maxM3u8FileSize {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf(
					"m3u8 file is too large: %d, max: %d (3MB)",
					resp.ContentLength,
					maxM3u8FileSize,
				),
			),
		)
		return fmt.Errorf(
			"m3u8 file is too large: %d, max: %d (3MB)",
			resp.ContentLength,
			maxM3u8FileSize,
		)
	}
	b, err := io.ReadAll(io.LimitReader(resp.Body, maxM3u8FileSize))
	if err != nil {
		ctx.AbortWithStatusJSON(http.StatusBadRequest,
			model.NewAPIErrorStringResp(
				fmt.Sprintf("read response body error: %v", err),
			),
		)
		return fmt.Errorf("read response body error: %w", err)
	}
	return M3u8Data(ctx, b, u, token, roomID, movieID)
}
