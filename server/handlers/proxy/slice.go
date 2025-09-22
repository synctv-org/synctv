package proxy

import (
	"crypto/sha256"
	"encoding/hex"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"

	"github.com/zijiren233/ksync"
	"github.com/zijiren233/stream"
)

var mu = ksync.DefaultKmutex()

// Proxy defines the interface for proxy implementations
type Proxy interface {
	io.ReadSeeker
	ContentTotalLength() (int64, error)
	ContentType() (string, error)
}

// Headers defines the interface for accessing response headers
type Headers interface {
	Headers() http.Header
}

// SliceCacheProxy implements caching of content slices
type SliceCacheProxy struct {
	r         Proxy
	cache     Cache
	key       string
	sliceSize int64
}

// NewSliceCacheProxy creates a new SliceCacheProxy instance
func NewSliceCacheProxy(key string, sliceSize int64, r Proxy, cache Cache) *SliceCacheProxy {
	return &SliceCacheProxy{
		key:       key,
		sliceSize: sliceSize,
		r:         r,
		cache:     cache,
	}
}

func cacheKey(key string, offset, sliceSize int64) string {
	hash := sha256.Sum256(stream.StringToBytes(key))
	return fmt.Sprintf("%s-%d-%d", hex.EncodeToString(hash[:]), sliceSize, offset)
}

func alignedOffset(offset, sliceSize int64) int64 {
	return (offset / sliceSize) * sliceSize
}

func fmtContentRange(start, end, total int64) string {
	if total == -1 && end == -1 {
		return "bytes */*"
	}

	totalStr := "*"
	if total >= 0 {
		totalStr = strconv.FormatInt(total, 10)
	}

	if end == -1 {
		if total >= 0 {
			end = total - 1
		}
		return fmt.Sprintf("bytes %d-%d/%s", start, end, totalStr)
	}

	return fmt.Sprintf("bytes %d-%d/%s", start, end, totalStr)
}

func contentLength(start, end, total int64) int64 {
	if total == -1 && end == -1 {
		return -1
	}

	if end == -1 {
		return total - start
	}

	if end >= total && total != -1 {
		return total - start
	}

	return end - start + 1
}

func fmtContentLength(start, end, total int64) string {
	length := contentLength(start, end, total)
	if length == -1 {
		return ""
	}

	return strconv.FormatInt(length, 10)
}

// ServeHTTP implements http.Handler interface
func (c *SliceCacheProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	_ = c.Proxy(w, r)
}

func (c *SliceCacheProxy) Proxy(w http.ResponseWriter, r *http.Request) error {
	byteRange, err := ParseByteRange(r.Header.Get("Range"))
	if err != nil {
		http.Error(w, fmt.Sprintf("Failed to parse Range header: %v", err), http.StatusBadRequest)
		return fmt.Errorf("failed to parse Range header: %w", err)
	}

	alignedOffset := alignedOffset(byteRange.Start, c.sliceSize)

	cacheItem, cached, err := c.getCacheItem(alignedOffset)
	if err != nil {
		http.Error(
			w,
			fmt.Sprintf("Failed to get cache item: %v", err),
			http.StatusInternalServerError,
		)

		return fmt.Errorf("failed to get cache item: %w", err)
	}

	c.setResponseHeaders(w, byteRange, cacheItem, cached, r.Header.Get("Range") != "")

	if err := c.writeResponse(w, byteRange, alignedOffset, cacheItem); err != nil {
		return fmt.Errorf("failed to write response: %w", err)
	}

	return nil
}

const cacheStatusHeader = "X-Cache-Status"

func (c *SliceCacheProxy) setResponseHeaders(
	w http.ResponseWriter,
	byteRange *ByteRange,
	cacheItem *CacheItem,
	cached, isRangeRequest bool,
) {
	// Copy headers excluding special ones
	for k, v := range cacheItem.Metadata.Headers {
		switch k {
		case "Content-Type", "Content-Length", "Content-Range", "Accept-Ranges":
			continue
		default:
			w.Header()[k] = v
		}
	}

	if cached {
		w.Header().Set(cacheStatusHeader, "HIT")
	} else {
		w.Header().Set(cacheStatusHeader, "MISS")
	}

	w.Header().Set("Accept-Ranges", "bytes")
	w.Header().
		Set("Content-Length", fmtContentLength(byteRange.Start, byteRange.End, cacheItem.Metadata.ContentTotalLength))
	w.Header().Set("Content-Type", cacheItem.Metadata.ContentType)

	if isRangeRequest {
		w.Header().
			Set("Content-Range", fmtContentRange(byteRange.Start, byteRange.End, cacheItem.Metadata.ContentTotalLength))
		w.WriteHeader(http.StatusPartialContent)
	} else {
		w.WriteHeader(http.StatusOK)
	}
}

func (c *SliceCacheProxy) writeResponse(
	w http.ResponseWriter,
	byteRange *ByteRange,
	alignedOffset int64,
	cacheItem *CacheItem,
) error {
	sliceOffset := byteRange.Start - alignedOffset
	if sliceOffset < 0 {
		return fmt.Errorf("slice offset cannot be negative, got: %d", sliceOffset)
	}

	remainingLength := contentLength(
		byteRange.Start,
		byteRange.End,
		cacheItem.Metadata.ContentTotalLength,
	)
	if remainingLength == 0 {
		return nil
	}

	// Write initial slice
	if remainingLength > 0 {
		n := int64(len(cacheItem.Data)) - sliceOffset
		if n > remainingLength {
			n = remainingLength
		}

		if n > 0 {
			if _, err := w.Write(cacheItem.Data[sliceOffset : sliceOffset+n]); err != nil {
				return fmt.Errorf("failed to write initial data slice: %w", err)
			}

			remainingLength -= n
		}
	}

	// Write subsequent slices
	currentOffset := alignedOffset + c.sliceSize
	for remainingLength > 0 {
		cacheItem, _, err := c.getCacheItem(currentOffset)
		if err != nil {
			return fmt.Errorf("failed to get cache item at offset %d: %w", currentOffset, err)
		}

		n := int64(len(cacheItem.Data))
		if n > remainingLength {
			n = remainingLength
		}

		if n > 0 {
			if _, err := w.Write(cacheItem.Data[:n]); err != nil {
				return fmt.Errorf("failed to write data slice at offset %d: %w", currentOffset, err)
			}

			remainingLength -= n
		}

		currentOffset += c.sliceSize
	}

	return nil
}

func (c *SliceCacheProxy) getCacheItem(alignedOffset int64) (*CacheItem, bool, error) {
	if alignedOffset < 0 {
		return nil, false, fmt.Errorf(
			"cache item offset cannot be negative, got: %d",
			alignedOffset,
		)
	}

	cacheKey := cacheKey(c.key, alignedOffset, c.sliceSize)

	mu.Lock(cacheKey)
	defer mu.Unlock(cacheKey)

	// Try to get from cache first
	slice, ok, err := c.cache.Get(cacheKey)
	if err != nil {
		return nil, false, fmt.Errorf("failed to get item from cache: %w", err)
	}

	if ok {
		return slice, true, nil
	}

	// Fetch from source if not in cache
	slice, err = c.fetchFromSource(alignedOffset)
	if err != nil {
		return nil, false, fmt.Errorf("failed to fetch item from source: %w", err)
	}

	// Store in cache
	if err = c.cache.Set(cacheKey, slice); err != nil {
		return nil, false, fmt.Errorf("failed to store item in cache: %w", err)
	}

	return slice, false, nil
}

func (c *SliceCacheProxy) contentTotalLength() (int64, error) {
	total, err := c.r.ContentTotalLength()
	if err != nil {
		return -1, fmt.Errorf("failed to get content total length from source: %w", err)
	}

	if total == -1 {
		return -1, errors.New("source does not support range requests")
	}

	return total, nil
}

func (c *SliceCacheProxy) fetchFromSource(offset int64) (*CacheItem, error) {
	if offset < 0 {
		return nil, fmt.Errorf("source offset cannot be negative, got: %d", offset)
	}

	if _, err := c.r.Seek(offset, io.SeekStart); err != nil {
		return nil, fmt.Errorf("failed to seek to offset %d in source: %w", offset, err)
	}

	var total int64 = -1

	buf := make([]byte, c.sliceSize)

	n, err := io.ReadFull(c.r, buf)
	if err != nil {
		if !errors.Is(err, io.ErrUnexpectedEOF) {
			return nil, fmt.Errorf(
				"failed to read %d bytes from source at offset %d: %w",
				c.sliceSize,
				offset,
				err,
			)
		}

		total, err = c.contentTotalLength()
		if err != nil {
			return nil, fmt.Errorf("failed to get content total length from source: %w", err)
		}

		if total != offset+int64(n) {
			return nil, fmt.Errorf(
				"source content total length mismatch, got: %d, expected: %d, %w",
				total,
				offset+int64(n),
				io.ErrUnexpectedEOF,
			)
		}
	}

	if total == -1 {
		total, err = c.contentTotalLength()
		if err != nil {
			return nil, fmt.Errorf("failed to get content total length from source: %w", err)
		}
	}

	contentType, err := c.r.ContentType()
	if err != nil {
		return nil, fmt.Errorf("failed to get content type from source: %w", err)
	}

	var headers http.Header
	if h, ok := c.r.(Headers); ok {
		headers = h.Headers().Clone()
	} else {
		headers = make(http.Header)
	}

	return &CacheItem{
		Metadata: &CacheMetadata{
			Headers:            headers,
			ContentTotalLength: total,
			ContentType:        contentType,
		},
		Data: buf[:n],
	}, nil
}

// ByteRange represents an HTTP Range header value
type ByteRange struct {
	Start int64
	End   int64
}

// ParseByteRange parses a Range header value in the format:
// bytes=<start>-<end>
// where end is optional
func ParseByteRange(r string) (*ByteRange, error) {
	if r == "" {
		return &ByteRange{Start: 0, End: -1}, nil
	}

	if !strings.HasPrefix(r, "bytes=") {
		return nil, fmt.Errorf("range header must start with 'bytes=', got: %s", r)
	}

	if strings.Contains(r, ",") {
		return nil, fmt.Errorf("not support multi-range, got: %s", r)
	}

	r = strings.TrimPrefix(r, "bytes=")

	parts := strings.Split(r, "-")
	if len(parts) != 2 {
		return nil, fmt.Errorf(
			"range header must contain exactly one hyphen (-) separator, got: %s",
			r,
		)
	}

	parts[0] = strings.TrimSpace(parts[0])
	parts[1] = strings.TrimSpace(parts[1])

	if parts[0] == "" && parts[1] == "" {
		return nil, fmt.Errorf("range header cannot have empty start and end values: %s", r)
	}

	var (
		start, end int64 = 0, -1
		err        error
	)

	if parts[0] != "" {
		start, err = strconv.ParseInt(parts[0], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("failed to parse range start value '%s': %w", parts[0], err)
		}

		if start < 0 {
			return nil, fmt.Errorf("range start value must be non-negative, got: %d", start)
		}
	}

	if parts[1] != "" {
		end, err = strconv.ParseInt(parts[1], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("failed to parse range end value '%s': %w", parts[1], err)
		}

		if end < 0 {
			return nil, fmt.Errorf("range end value must be non-negative, got: %d", end)
		}

		if start > end {
			return nil, fmt.Errorf(
				"range start value (%d) cannot be greater than end value (%d)",
				start,
				end,
			)
		}
	}

	return &ByteRange{Start: start, End: end}, nil
}
