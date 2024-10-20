package proxy

import (
	"errors"
	"fmt"
	"net/http"
	"strconv"
	"strings"
)

type ByteRange struct {
	Start int64
	End   int64
}

// Range: <unit>=<range-start>-<range-end>
// Range: bytes=0-999
// Range: bytes=-500
func ParseRange(rangeStr string) (*ByteRange, error) {
	if !strings.HasPrefix(rangeStr, "bytes=") {
		return nil, errors.New("invalid range format")
	}

	rangeStr = strings.TrimPrefix(rangeStr, "bytes=")
	parts := strings.Split(rangeStr, "-")

	if len(parts) != 2 {
		return nil, errors.New("invalid range format")
	}

	var start, end int64
	var err error

	if parts[0] == "" {
		// Range: bytes=-500
		end, err = strconv.ParseInt(parts[1], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid range end: %w", err)
		}
		start = -1 // 表示从文件末尾开始计算
	} else {
		start, err = strconv.ParseInt(parts[0], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid range start: %w", err)
		}

		if parts[1] == "" {
			// Range: bytes=1000-
			end = -1 // 表示到文件末尾
		} else {
			end, err = strconv.ParseInt(parts[1], 10, 64)
			if err != nil {
				return nil, fmt.Errorf("invalid range end: %w", err)
			}
		}
	}

	return &ByteRange{Start: start, End: end}, nil
}

// Content-Type: multipart/byteranges
// Range: bytes=0-999, 2000-2999
func ParseMultipartByteRanges(rangeStr string) ([]*ByteRange, error) {
	if !strings.HasPrefix(rangeStr, "bytes=") {
		return nil, errors.New("invalid range format")
	}

	rangeStr = strings.TrimPrefix(rangeStr, "bytes=")
	ranges := strings.Split(rangeStr, ",")

	byteRanges := make([]*ByteRange, 0, len(ranges))
	for _, r := range ranges {
		byteRange, err := ParseRange(strings.TrimSpace(r))
		if err != nil {
			return nil, err
		}
		byteRanges = append(byteRanges, byteRange)
	}

	return byteRanges, nil
}

func headerMapToHeader(headers map[string]string) http.Header {
	h := make(http.Header)
	for k, v := range headers {
		h.Set(k, v)
	}
	return h
}

func ParseContentRange(contentRange string) (*ByteRange, error) {
	// Content-Range: bytes 200-1000/67589
	parts := strings.Fields(contentRange)
	if len(parts) != 2 || parts[0] != "bytes" {
		return nil, errors.New("invalid Content-Range format")
	}

	rangeParts := strings.Split(parts[1], "/")
	if len(rangeParts) != 2 {
		return nil, errors.New("invalid Content-Range format")
	}

	rangeParts = strings.Split(rangeParts[0], "-")
	if len(rangeParts) != 2 {
		return nil, errors.New("invalid Content-Range format")
	}

	start, err := strconv.ParseInt(rangeParts[0], 10, 64)
	if err != nil {
		return nil, fmt.Errorf("invalid Content-Range start: %w", err)
	}

	end, err := strconv.ParseInt(rangeParts[1], 10, 64)
	if err != nil {
		return nil, fmt.Errorf("invalid Content-Range end: %w", err)
	}

	return &ByteRange{Start: start, End: end}, nil
}
