package proxy

import (
	"fmt"
	"io"
	"sync"

	"github.com/zijiren233/ksync"
)

type Cache interface {
	Get(key string) ([]byte, bool, error)
	Set(key string, data []byte) error
}

type MemoryCache struct {
	mu sync.RWMutex
	m  map[string][]byte
}

func NewMemoryCache() *MemoryCache {
	return &MemoryCache{
		m: make(map[string][]byte),
	}
}

func (c *MemoryCache) Get(key string) ([]byte, bool, error) {
	c.mu.RLock()
	defer c.mu.RUnlock()
	data, ok := c.m[key]
	return data, ok, nil
}

func (c *MemoryCache) Set(key string, data []byte) error {
	c.mu.Lock()
	defer c.mu.Unlock()
	c.m[key] = data
	return nil
}

var mu = ksync.DefaultKmutex()

type CachedReadSeeker struct {
	key       string
	sliceSize int64
	offset    int64
	r         io.ReadSeeker
	cache     Cache
}

func NewCachedReadSeeker(key string, sliceSize int64, r io.ReadSeeker, cache Cache) *CachedReadSeeker {
	return &CachedReadSeeker{
		key:       key,
		sliceSize: sliceSize,
		r:         r,
		cache:     cache,
	}
}

func (c *CachedReadSeeker) cacheKey(offset int64) string {
	return fmt.Sprintf("%s-%d-%d", c.key, offset, c.sliceSize)
}

func (c *CachedReadSeeker) alignedOffset() int64 {
	return (c.offset / c.sliceSize) * c.sliceSize
}

func (c *CachedReadSeeker) Seek(offset int64, whence int) (int64, error) {
	newOffset, err := c.r.Seek(offset, whence)
	if err == nil {
		c.offset = newOffset
	}
	return newOffset, err
}

func (c *CachedReadSeeker) Read(p []byte) (n int, err error) {
	for n < len(p) {
		alignedOffset := c.alignedOffset()

		slice, err := c.getSlice(alignedOffset)
		if err != nil {
			return n, err
		}

		sliceOffset := int(c.offset - alignedOffset)
		copied := copy(p[n:], slice[sliceOffset:])
		c.offset += int64(copied)
		n += copied

		if copied == 0 {
			if n == 0 {
				return 0, io.EOF
			}
			break
		}
	}

	return n, nil
}

func (c *CachedReadSeeker) getSlice(alignedOffset int64) ([]byte, error) {
	cacheKey := c.cacheKey(alignedOffset)
	mu.Lock(cacheKey)
	defer mu.Unlock(cacheKey)

	slice, ok, err := c.cache.Get(cacheKey)
	if err != nil {
		return nil, err
	}
	if ok {
		return slice, nil
	}

	slice, err = c.fetchFromSource(alignedOffset)
	if err != nil {
		return nil, err
	}

	if err = c.cache.Set(cacheKey, slice); err != nil {
		return nil, err
	}

	return slice, nil
}

func (c *CachedReadSeeker) fetchFromSource(offset int64) ([]byte, error) {
	if _, err := c.r.Seek(offset, io.SeekStart); err != nil {
		return nil, err
	}

	buf := make([]byte, c.sliceSize)
	n, err := io.ReadFull(c.r, buf)
	if err != nil && err != io.ErrUnexpectedEOF {
		return nil, err
	}

	return buf[:n], nil
}

type CachedReader struct {
	key       string
	sliceSize int64
	offset    int64
	r         io.Reader
	cache     Cache
}

func NewCachedReader(key string, sliceSize, offset int64, r io.Reader, cache Cache) *CachedReader {
	return &CachedReader{
		key:       key,
		sliceSize: sliceSize,
		offset:    offset,
		r:         r,
		cache:     cache,
	}
}

func (c *CachedReader) cacheKey(offset int64) string {
	return fmt.Sprintf("%s-%d-%d", c.key, offset, c.sliceSize)
}

func (c *CachedReader) alignedOffset() int64 {
	return (c.offset / c.sliceSize) * c.sliceSize
}

func (c *CachedReader) Read(p []byte) (n int, err error) {
	for n < len(p) {
		alignedOffset := c.alignedOffset()
		slice, err := c.getSlice(alignedOffset)
		if err != nil {
			if err == ErrOffsetNotAligned {
				// Read directly from source until next aligned offset
				bytesToNextAligned := c.sliceSize - (c.offset % c.sliceSize)
				toRead := min(int64(len(p)-n), bytesToNextAligned)
				read, err := io.ReadAtLeast(c.r, p[n:n+int(toRead)], int(toRead))
				c.offset += int64(read)
				n += read
				if err != nil {
					if err == io.EOF && n > 0 {
						return n, nil
					}
					return n, err
				}
				continue
			}
			return n, err
		}

		sliceOffset := int(c.offset - alignedOffset)
		if sliceOffset >= len(slice) {
			sliceOffset = len(slice) - 1
		}
		copied := copy(p[n:], slice[sliceOffset:])
		c.offset += int64(copied)
		n += copied

		if copied == 0 {
			break
		}
	}
	if n == 0 {
		return 0, io.EOF
	}
	return n, nil
}

var ErrOffsetNotAligned = fmt.Errorf("offset is not aligned with slice size")

func (c *CachedReader) getSlice(alignedOffset int64) ([]byte, error) {
	cacheKey := c.cacheKey(alignedOffset)
	mu.Lock(cacheKey)
	defer mu.Unlock(cacheKey)

	slice, ok, err := c.cache.Get(cacheKey)
	if err != nil {
		return nil, err
	}
	if ok {
		return slice, nil
	}

	if c.offset != alignedOffset {
		return nil, ErrOffsetNotAligned
	}

	slice, err = c.fetchFromSource()
	if err != nil {
		return nil, err
	}

	if err = c.cache.Set(cacheKey, slice); err != nil {
		return nil, err
	}

	return slice, nil
}

func (c *CachedReader) fetchFromSource() ([]byte, error) {
	buf := make([]byte, c.sliceSize)
	n, err := io.ReadFull(c.r, buf)
	if err != nil && err != io.ErrUnexpectedEOF {
		return nil, err
	}
	return buf[:n], nil
}
