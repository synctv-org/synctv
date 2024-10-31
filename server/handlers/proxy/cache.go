package proxy

import (
	"encoding/binary"
	"fmt"
	"io"
	"net/http"
	"strconv"
	"strings"
	"sync"

	json "github.com/json-iterator/go"
	"github.com/zijiren233/ksync"
)

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
		return nil, fmt.Errorf("invalid range prefix: %s", r)
	}

	r = strings.TrimPrefix(r, "bytes=")
	parts := strings.Split(r, "-")
	if len(parts) != 2 {
		return nil, fmt.Errorf("invalid range format: %s", r)
	}

	parts[0] = strings.TrimSpace(parts[0])
	parts[1] = strings.TrimSpace(parts[1])

	if parts[0] == "" && parts[1] == "" {
		return nil, fmt.Errorf("empty range values")
	}

	var start, end int64 = 0, -1
	var err error

	if parts[0] != "" {
		start, err = strconv.ParseInt(parts[0], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid range start: %s", parts[0])
		}
		if start < 0 {
			return nil, fmt.Errorf("negative range start: %d", start)
		}
	}

	if parts[1] != "" {
		end, err = strconv.ParseInt(parts[1], 10, 64)
		if err != nil {
			return nil, fmt.Errorf("invalid range end: %s", parts[1])
		}
		if end < 0 {
			return nil, fmt.Errorf("negative range end: %d", end)
		}
		if start > end {
			return nil, fmt.Errorf("invalid range: start (%d) greater than end (%d)", start, end)
		}
	}

	return &ByteRange{Start: start, End: end}, nil
}

// CacheMetadata stores metadata about a cached response
type CacheMetadata struct {
	Headers            http.Header `json:"headers"` // Excludes content-type, content-range, content-length
	ContentType        string      `json:"content_type"`
	ContentTotalLength int64       `json:"content_total_length"`
}

func (m *CacheMetadata) MarshalBinary() ([]byte, error) {
	if m == nil {
		return nil, fmt.Errorf("nil metadata")
	}
	return json.Marshal(m)
}

// CacheItem represents a cached response with metadata and data
type CacheItem struct {
	Metadata *CacheMetadata
	Data     []byte
}

// WriteTo implements io.WriterTo to serialize the cache item
func (i *CacheItem) WriteTo(w io.Writer) (int64, error) {
	if i == nil {
		return 0, fmt.Errorf("nil cache item")
	}

	if i.Metadata == nil {
		return 0, fmt.Errorf("nil metadata")
	}

	if w == nil {
		return 0, fmt.Errorf("nil writer")
	}

	metadata, err := i.Metadata.MarshalBinary()
	if err != nil {
		return 0, fmt.Errorf("marshal metadata: %w", err)
	}

	var written int64

	// Write metadata length and metadata
	if err := binary.Write(w, binary.BigEndian, int64(len(metadata))); err != nil {
		return written, fmt.Errorf("write metadata length: %w", err)
	}
	written += 8

	n, err := w.Write(metadata)
	written += int64(n)
	if err != nil {
		return written, fmt.Errorf("write metadata: %w", err)
	}

	// Write data length and data
	if err := binary.Write(w, binary.BigEndian, int64(len(i.Data))); err != nil {
		return written, fmt.Errorf("write data length: %w", err)
	}
	written += 8

	n, err = w.Write(i.Data)
	written += int64(n)
	if err != nil {
		return written, fmt.Errorf("write data: %w", err)
	}

	return written, nil
}

// ReadFrom implements io.ReaderFrom to deserialize the cache item
func (i *CacheItem) ReadFrom(r io.Reader) (int64, error) {
	if i == nil {
		return 0, fmt.Errorf("nil cache item")
	}

	if r == nil {
		return 0, fmt.Errorf("nil reader")
	}

	var read int64

	// Read metadata length and metadata
	var metadataLen int64
	if err := binary.Read(r, binary.BigEndian, &metadataLen); err != nil {
		return read, fmt.Errorf("read metadata length: %w", err)
	}
	read += 8

	if metadataLen <= 0 {
		return read, fmt.Errorf("invalid metadata length: %d", metadataLen)
	}

	metadata := make([]byte, metadataLen)
	n, err := io.ReadFull(r, metadata)
	read += int64(n)
	if err != nil {
		return read, fmt.Errorf("read metadata: %w", err)
	}

	i.Metadata = new(CacheMetadata)
	if err := json.Unmarshal(metadata, i.Metadata); err != nil {
		return read, fmt.Errorf("unmarshal metadata: %w", err)
	}

	// Read data length and data
	var dataLen int64
	if err := binary.Read(r, binary.BigEndian, &dataLen); err != nil {
		return read, fmt.Errorf("read data length: %w", err)
	}
	read += 8

	if dataLen < 0 {
		return read, fmt.Errorf("invalid data length: %d", dataLen)
	}

	i.Data = make([]byte, dataLen)
	n, err = io.ReadFull(r, i.Data)
	read += int64(n)
	if err != nil {
		return read, fmt.Errorf("read data: %w", err)
	}

	return read, nil
}

// Cache defines the interface for cache implementations
type Cache interface {
	Get(key string) (*CacheItem, bool, error)
	Set(key string, data *CacheItem) error
}

var defaultCache Cache = NewMemoryCache()

// MemoryCache implements an in-memory Cache with thread-safe operations
type MemoryCache struct {
	m  map[string]*CacheItem
	mu sync.RWMutex
}

func NewMemoryCache() *MemoryCache {
	return &MemoryCache{
		m: make(map[string]*CacheItem),
	}
}

func (c *MemoryCache) Get(key string) (*CacheItem, bool, error) {
	if key == "" {
		return nil, false, fmt.Errorf("empty key")
	}

	c.mu.RLock()
	defer c.mu.RUnlock()
	item, ok := c.m[key]
	if !ok {
		return nil, false, nil
	}
	return item, true, nil
}

func (c *MemoryCache) Set(key string, data *CacheItem) error {
	if key == "" {
		return fmt.Errorf("empty key")
	}
	if data == nil {
		return fmt.Errorf("nil cache item")
	}

	c.mu.Lock()
	defer c.mu.Unlock()
	c.m[key] = data
	return nil
}

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

func (c *SliceCacheProxy) cacheKey(offset int64) string {
	return fmt.Sprintf("%s-%d-%d", c.key, offset, c.sliceSize)
}

func (c *SliceCacheProxy) alignedOffset(offset int64) int64 {
	return (offset / c.sliceSize) * c.sliceSize
}

func (c *SliceCacheProxy) fmtContentRange(start, end, total int64) string {
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

func (c *SliceCacheProxy) contentLength(start, end, total int64) int64 {
	if total == -1 && end == -1 {
		return -1
	}
	if end == -1 {
		if total == -1 {
			return -1
		}
		return total - start
	}
	if end >= total && total != -1 {
		return total - start
	}
	return end - start + 1
}

func (c *SliceCacheProxy) fmtContentLength(start, end, total int64) string {
	length := c.contentLength(start, end, total)
	if length == -1 {
		return ""
	}
	return strconv.FormatInt(length, 10)
}

// ServeHTTP implements http.Handler interface
func (c *SliceCacheProxy) ServeHTTP(w http.ResponseWriter, r *http.Request) {
	byteRange, err := ParseByteRange(r.Header.Get("Range"))
	if err != nil {
		http.Error(w, err.Error(), http.StatusBadRequest)
		return
	}

	alignedOffset := c.alignedOffset(byteRange.Start)
	cacheItem, err := c.getCacheItem(alignedOffset)
	if err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}

	c.setResponseHeaders(w, byteRange, cacheItem)
	if err := c.writeResponse(w, byteRange, alignedOffset, cacheItem); err != nil {
		http.Error(w, err.Error(), http.StatusInternalServerError)
		return
	}
}

func (c *SliceCacheProxy) setResponseHeaders(w http.ResponseWriter, byteRange *ByteRange, cacheItem *CacheItem) {
	contentRange := c.fmtContentRange(byteRange.Start, byteRange.End, cacheItem.Metadata.ContentTotalLength)
	w.Header().Set("Content-Type", cacheItem.Metadata.ContentType)

	// Copy headers excluding special ones
	for k, v := range cacheItem.Metadata.Headers {
		switch k {
		case "Content-Type", "Content-Length", "Content-Range", "Accept-Ranges":
			continue
		default:
			w.Header()[k] = v
		}
	}

	w.Header().Set("Accept-Ranges", "bytes")
	w.Header().Set("Content-Length", c.fmtContentLength(byteRange.Start, byteRange.End, cacheItem.Metadata.ContentTotalLength))
	w.Header().Set("Content-Range", contentRange)

	w.WriteHeader(http.StatusPartialContent)
}

func (c *SliceCacheProxy) writeResponse(w http.ResponseWriter, byteRange *ByteRange, alignedOffset int64, cacheItem *CacheItem) error {
	if w == nil || byteRange == nil || cacheItem == nil {
		return fmt.Errorf("nil parameters")
	}

	sliceOffset := byteRange.Start - alignedOffset
	if sliceOffset < 0 {
		return fmt.Errorf("negative slice offset")
	}

	remainingLength := c.contentLength(byteRange.Start, byteRange.End, cacheItem.Metadata.ContentTotalLength)
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
				return fmt.Errorf("write initial slice: %w", err)
			}
			remainingLength -= n
		}
	}

	// Write subsequent slices
	currentOffset := alignedOffset + c.sliceSize
	for remainingLength > 0 {
		cacheItem, err := c.getCacheItem(currentOffset)
		if err != nil {
			return fmt.Errorf("get cache item: %w", err)
		}

		n := int64(len(cacheItem.Data))
		if n > remainingLength {
			n = remainingLength
		}
		if n > 0 {
			if _, err := w.Write(cacheItem.Data[:n]); err != nil {
				return fmt.Errorf("write slice: %w", err)
			}
			remainingLength -= n
		}
		currentOffset += c.sliceSize
	}

	return nil
}

func (c *SliceCacheProxy) getCacheItem(alignedOffset int64) (*CacheItem, error) {
	if alignedOffset < 0 {
		return nil, fmt.Errorf("negative offset")
	}

	cacheKey := c.cacheKey(alignedOffset)
	mu.Lock(cacheKey)
	defer mu.Unlock(cacheKey)

	// Try to get from cache first
	slice, ok, err := c.cache.Get(cacheKey)
	if err != nil {
		return nil, fmt.Errorf("get from cache: %w", err)
	}
	if ok {
		return slice, nil
	}

	// Fetch from source if not in cache
	slice, err = c.fetchFromSource(alignedOffset)
	if err != nil {
		return nil, fmt.Errorf("fetch from source: %w", err)
	}

	// Store in cache
	if err = c.cache.Set(cacheKey, slice); err != nil {
		return nil, fmt.Errorf("set cache: %w", err)
	}

	return slice, nil
}

func (c *SliceCacheProxy) fetchFromSource(offset int64) (*CacheItem, error) {
	if offset < 0 {
		return nil, fmt.Errorf("negative offset")
	}
	if _, err := c.r.Seek(offset, io.SeekStart); err != nil {
		return nil, fmt.Errorf("seek source: %w", err)
	}

	buf := make([]byte, c.sliceSize)
	n, err := io.ReadFull(c.r, buf)
	if err != nil && err != io.ErrUnexpectedEOF {
		return nil, fmt.Errorf("read source: %w", err)
	}

	var headers http.Header
	if h, ok := c.r.(Headers); ok {
		headers = h.Headers().Clone()
	} else {
		headers = make(http.Header)
	}

	contentTotalLength, err := c.r.ContentTotalLength()
	if err != nil {
		return nil, fmt.Errorf("get content total length: %w", err)
	}

	contentType, err := c.r.ContentType()
	if err != nil {
		return nil, fmt.Errorf("get content type: %w", err)
	}

	return &CacheItem{
		Metadata: &CacheMetadata{
			Headers:            headers,
			ContentTotalLength: contentTotalLength,
			ContentType:        contentType,
		},
		Data: buf[:n],
	}, nil
}
