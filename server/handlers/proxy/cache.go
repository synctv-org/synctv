package proxy

import (
	"encoding/binary"
	"fmt"
	"io"
	"net/http"
	"os"
	"path/filepath"
	"sort"
	"sync"
	"sync/atomic"
	"time"

	json "github.com/json-iterator/go"
	"github.com/zijiren233/gencontainer/dllist"
	"github.com/zijiren233/ksync"
)

// Cache defines the interface for cache implementations
type Cache interface {
	Get(key string) (*CacheItem, bool, error)
	Set(key string, data *CacheItem) error
}

// CacheMetadata stores metadata about a cached response
type CacheMetadata struct {
	Headers            http.Header `json:"headers"`
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

	// Write metadata length and content
	if err := binary.Write(w, binary.BigEndian, int64(len(metadata))); err != nil {
		return written, fmt.Errorf("failed to write metadata length: %w", err)
	}
	written += 8

	n, err := w.Write(metadata)
	written += int64(n)
	if err != nil {
		return written, fmt.Errorf("failed to write metadata bytes: %w", err)
	}

	// Write data length and content
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

	// Read metadata length and content
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

	// Read data length and content
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

// MemoryCache implements an in-memory Cache with LRU eviction
type MemoryCache struct {
	m            map[string]*dllist.Element[*cacheEntry]
	lruList      *dllist.Dllist[*cacheEntry]
	capacity     int
	maxSizeBytes int64
	currentSize  int64
	mu           sync.RWMutex
}

type MemoryCacheOption func(*MemoryCache)

func WithMaxSizeBytes(size int64) MemoryCacheOption {
	return func(c *MemoryCache) {
		c.maxSizeBytes = size
	}
}

type cacheEntry struct {
	item *CacheItem
	key  string
	size int64
}

func NewMemoryCache(capacity int, opts ...MemoryCacheOption) *MemoryCache {
	mc := &MemoryCache{
		m:        make(map[string]*dllist.Element[*cacheEntry]),
		lruList:  dllist.New[*cacheEntry](),
		capacity: capacity,
	}
	for _, opt := range opts {
		opt(mc)
	}
	return mc
}

func (c *MemoryCache) Get(key string) (*CacheItem, bool, error) {
	if key == "" {
		return nil, false, fmt.Errorf("cache key cannot be empty")
	}

	c.mu.RLock()
	element, exists := c.m[key]
	if !exists {
		c.mu.RUnlock()
		return nil, false, nil
	}

	// Upgrade to write lock for moving element
	c.mu.RUnlock()
	c.mu.Lock()
	c.lruList.MoveToFront(element)
	item := element.Value.item
	c.mu.Unlock()

	return item, true, nil
}

func (c *MemoryCache) Set(key string, data *CacheItem) error {
	if key == "" {
		return fmt.Errorf("cache key cannot be empty")
	}
	if data == nil {
		return fmt.Errorf("cannot cache nil CacheItem")
	}

	// Calculate size of new item
	newSize := int64(len(data.Data))
	if data.Metadata != nil {
		metadataBytes, err := data.Metadata.MarshalBinary()
		if err == nil {
			newSize += int64(len(metadataBytes))
		}
	}

	c.mu.Lock()
	defer c.mu.Unlock()

	// Update existing entry if present
	if element, ok := c.m[key]; ok {
		c.currentSize -= element.Value.size
		c.currentSize += newSize
		c.lruList.MoveToFront(element)
		element.Value.item = data
		element.Value.size = newSize
		return nil
	}

	// Evict entries if needed
	for c.lruList.Len() > 0 &&
		((c.capacity > 0 && c.lruList.Len() >= c.capacity) ||
			(c.maxSizeBytes > 0 && c.currentSize+newSize > c.maxSizeBytes)) {

		if back := c.lruList.Back(); back != nil {
			entry := back.Value
			c.currentSize -= entry.size
			delete(c.m, entry.key)
			c.lruList.Remove(back)
		}
	}

	// Add new entry
	newEntry := &cacheEntry{key: key, item: data, size: newSize}
	element := c.lruList.PushFront(newEntry)
	c.m[key] = element
	c.currentSize += newSize
	return nil
}

type FileCache struct {
	mu           *ksync.Krwmutex
	filePath     string
	maxSizeBytes int64
	currentSize  atomic.Int64
	lastCleanup  atomic.Int64
	maxAge       time.Duration
	cleanMu      sync.Mutex
}

type FileCacheOption func(*FileCache)

func WithFileCacheMaxSizeBytes(size int64) FileCacheOption {
	return func(c *FileCache) {
		c.maxSizeBytes = size
	}
}

func WithFileCacheMaxAge(age time.Duration) FileCacheOption {
	return func(c *FileCache) {
		if age > 0 {
			c.maxAge = age
		}
	}
}

func NewFileCache(filePath string, opts ...FileCacheOption) *FileCache {
	fc := &FileCache{
		filePath: filePath,
		mu:       ksync.DefaultKrwmutex(),
		maxAge:   24 * time.Hour, // Default 1 day
	}

	for _, opt := range opts {
		opt(fc)
	}

	go fc.periodicCleanup()
	return fc
}

func (c *FileCache) periodicCleanup() {
	ticker := time.NewTicker(5 * time.Minute)
	defer ticker.Stop()

	for range ticker.C {
		c.cleanup()
	}
}

func (c *FileCache) cleanup() {
	maxSize := c.maxSizeBytes
	if maxSize <= 0 {
		return
	}

	// Avoid frequent cleanups
	now := time.Now().Unix()
	if now-c.lastCleanup.Load() < 300 {
		return
	}

	c.cleanMu.Lock()
	defer c.cleanMu.Unlock()

	// Double check after acquiring lock
	if now-c.lastCleanup.Load() < 300 {
		return
	}

	entries, err := os.ReadDir(c.filePath)
	if err != nil {
		return
	}

	type fileInfo struct {
		modTime time.Time
		path    string
		size    int64
	}

	var files []fileInfo
	var totalSize int64
	cutoffTime := time.Now().Add(-c.maxAge)

	// Collect file information and remove expired files
	for _, entry := range entries {
		if !entry.IsDir() {
			continue
		}

		subdir := filepath.Join(c.filePath, entry.Name())
		subEntries, err := os.ReadDir(subdir)
		if err != nil {
			continue
		}

		for _, subEntry := range subEntries {
			info, err := subEntry.Info()
			if err != nil {
				continue
			}

			fullPath := filepath.Join(subdir, subEntry.Name())

			// Remove expired files
			if info.ModTime().Before(cutoffTime) {
				os.Remove(fullPath)
				continue
			}

			files = append(files, fileInfo{
				path:    fullPath,
				size:    info.Size(),
				modTime: info.ModTime(),
			})
			totalSize += info.Size()
		}
	}

	// If under size limit, just update size and return
	if totalSize <= maxSize {
		c.currentSize.Store(totalSize)
		c.lastCleanup.Store(now)
		return
	}

	// Sort by modification time (oldest first) and remove until under limit
	sort.Slice(files, func(i, j int) bool {
		return files[i].modTime.Before(files[j].modTime)
	})

	for _, file := range files {
		if totalSize <= maxSize {
			break
		}
		if err := os.Remove(file.path); err == nil {
			totalSize -= file.size
		}
	}

	c.currentSize.Store(totalSize)
	c.lastCleanup.Store(now)
}

func (c *FileCache) Get(key string) (*CacheItem, bool, error) {
	if key == "" {
		return nil, false, fmt.Errorf("cache key cannot be empty")
	}

	prefix := string(key[0])
	filePath := filepath.Join(c.filePath, prefix, key)

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

	// Check if file is expired
	if info, err := file.Stat(); err == nil {
		if time.Since(info.ModTime()) > c.maxAge {
			os.Remove(filePath)
			return nil, false, nil
		}
	}

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

	// Check and cleanup if needed
	maxSize := c.maxSizeBytes
	if maxSize > 0 {
		newSize := int64(len(data.Data))
		if data.Metadata != nil {
			if metadataBytes, err := data.Metadata.MarshalBinary(); err == nil {
				newSize += int64(len(metadataBytes))
			}
		}
		if c.currentSize.Load()+newSize > maxSize {
			c.cleanup()
		}
	}

	prefix := string(key[0])
	dirPath := filepath.Join(c.filePath, prefix)
	filePath := filepath.Join(dirPath, key)

	c.mu.Lock(key)
	defer c.mu.Unlock(key)

	if err := os.MkdirAll(dirPath, 0o755); err != nil {
		return fmt.Errorf("failed to create cache directory: %w", err)
	}

	file, err := os.OpenFile(filePath, os.O_WRONLY|os.O_CREATE|os.O_TRUNC, 0o644)
	if err != nil {
		return fmt.Errorf("failed to create cache file: %w", err)
	}
	defer file.Close()

	if _, err := data.WriteTo(file); err != nil {
		return fmt.Errorf("failed to write cache item: %w", err)
	}

	if info, err := file.Stat(); err == nil {
		c.currentSize.Add(info.Size())
	}

	return nil
}
