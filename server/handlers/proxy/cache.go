package proxy

import (
	"encoding/binary"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sync"

	json "github.com/json-iterator/go"
	"github.com/zijiren233/ksync"
)

// Cache defines the interface for cache implementations
type Cache interface {
	Get(key string) (*CacheItem, bool, error)
	Set(key string, data *CacheItem) error
}

// CacheMetadata stores metadata about a cached response
type CacheMetadata struct {
	Headers            http.Header `json:"headers"` // Excludes content-type, content-range, content-length
	ContentType        string      `json:"content_type"`
	ContentTotalLength int64       `json:"content_total_length"`
}

func (m *CacheMetadata) MarshalBinary() ([]byte, error) {
	return json.Marshal(m)
}

// CacheItem represents a cached response with metadata and data
type CacheItem struct {
	Metadata *CacheMetadata
	Data     []byte
}

// WriteTo implements io.WriterTo to serialize the cache item
func (i *CacheItem) WriteTo(w io.Writer) (int64, error) {
	if w == nil {
		return 0, fmt.Errorf("cannot write to nil io.Writer")
	}

	if i.Metadata == nil {
		return 0, fmt.Errorf("CacheItem contains nil Metadata")
	}

	metadata, err := i.Metadata.MarshalBinary()
	if err != nil {
		return 0, fmt.Errorf("failed to marshal metadata: %w", err)
	}

	var written int64

	// Write metadata length and metadata
	if err := binary.Write(w, binary.BigEndian, int64(len(metadata))); err != nil {
		return written, fmt.Errorf("failed to write metadata length: %w", err)
	}
	written += 8

	n, err := w.Write(metadata)
	written += int64(n)
	if err != nil {
		return written, fmt.Errorf("failed to write metadata bytes: %w", err)
	}

	// Write data length and data
	if err := binary.Write(w, binary.BigEndian, int64(len(i.Data))); err != nil {
		return written, fmt.Errorf("failed to write data length: %w", err)
	}
	written += 8

	n, err = w.Write(i.Data)
	written += int64(n)
	if err != nil {
		return written, fmt.Errorf("failed to write data bytes: %w", err)
	}

	return written, nil
}

// ReadFrom implements io.ReaderFrom to deserialize the cache item
func (i *CacheItem) ReadFrom(r io.Reader) (int64, error) {
	if r == nil {
		return 0, fmt.Errorf("cannot read from nil io.Reader")
	}

	var read int64

	// Read metadata length and metadata
	var metadataLen int64
	if err := binary.Read(r, binary.BigEndian, &metadataLen); err != nil {
		return read, fmt.Errorf("failed to read metadata length: %w", err)
	}
	read += 8

	if metadataLen <= 0 {
		return read, fmt.Errorf("metadata length must be positive, got: %d", metadataLen)
	}

	metadata := make([]byte, metadataLen)
	n, err := io.ReadFull(r, metadata)
	read += int64(n)
	if err != nil {
		return read, fmt.Errorf("failed to read metadata bytes: %w", err)
	}

	i.Metadata = new(CacheMetadata)
	if err := json.Unmarshal(metadata, i.Metadata); err != nil {
		return read, fmt.Errorf("failed to unmarshal metadata: %w", err)
	}

	// Read data length and data
	var dataLen int64
	if err := binary.Read(r, binary.BigEndian, &dataLen); err != nil {
		return read, fmt.Errorf("failed to read data length: %w", err)
	}
	read += 8

	if dataLen < 0 {
		return read, fmt.Errorf("data length cannot be negative, got: %d", dataLen)
	}

	i.Data = make([]byte, dataLen)
	n, err = io.ReadFull(r, i.Data)
	read += int64(n)
	if err != nil {
		return read, fmt.Errorf("failed to read data bytes: %w", err)
	}

	return read, nil
}

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
		return nil, false, fmt.Errorf("cache key cannot be empty")
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
		return fmt.Errorf("cache key cannot be empty")
	}
	if data == nil {
		return fmt.Errorf("cannot cache nil CacheItem")
	}

	c.mu.Lock()
	defer c.mu.Unlock()
	c.m[key] = data
	return nil
}

type FileCache struct {
	mu       *ksync.Krwmutex
	filePath string
}

func NewFileCache(filePath string) *FileCache {
	return &FileCache{filePath: filePath, mu: ksync.DefaultKrwmutex()}
}

func (c *FileCache) Get(key string) (*CacheItem, bool, error) {
	if key == "" {
		return nil, false, fmt.Errorf("cache key cannot be empty")
	}

	filePath := filepath.Join(c.filePath, key)

	c.mu.RLock(key)
	defer c.mu.RUnlock(key)

	file, err := os.OpenFile(filePath, os.O_RDONLY, 0o644)
	if err != nil {
		if os.IsNotExist(err) {
			return nil, false, nil
		}
		return nil, false, fmt.Errorf("failed to open cache file: %w", err)
	}
	defer file.Close()

	item := &CacheItem{}
	if _, err := item.ReadFrom(file); err != nil {
		return nil, false, fmt.Errorf("failed to read cache item: %w", err)
	}

	return item, true, nil
}

func (c *FileCache) Set(key string, data *CacheItem) error {
	if key == "" {
		return fmt.Errorf("cache key cannot be empty")
	}
	if data == nil {
		return fmt.Errorf("cannot cache nil CacheItem")
	}

	c.mu.Lock(key)
	defer c.mu.Unlock(key)

	if err := os.MkdirAll(c.filePath, 0o755); err != nil {
		return fmt.Errorf("failed to create cache directory: %w", err)
	}

	filePath := filepath.Join(c.filePath, key)
	file, err := os.OpenFile(filePath, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o644)
	if err != nil {
		return fmt.Errorf("failed to create cache file: %w", err)
	}
	defer file.Close()

	if _, err := data.WriteTo(file); err != nil {
		return fmt.Errorf("failed to write cache item: %w", err)
	}

	return nil
}
