package proxy

import (
	"sync"
)

const (
	DefaultBufferSize = 16 * 1024
)

var sharedBufferPool = sync.Pool{
	New: func() interface{} {
		buffer := make([]byte, DefaultBufferSize)
		return &buffer
	},
}

func getBuffer() *[]byte {
	return sharedBufferPool.Get().(*[]byte)
}

func putBuffer(buffer *[]byte) {
	sharedBufferPool.Put(buffer)
}
