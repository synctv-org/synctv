package proxy

import (
	"errors"
	"io"
	"sync"
)

const (
	DefaultBufferSize = 16 * 1024
)

var sharedBufferPool = sync.Pool{
	New: func() any {
		buffer := make([]byte, DefaultBufferSize)
		return &buffer
	},
}

func getBuffer() *[]byte {
	buf, ok := sharedBufferPool.Get().(*[]byte)
	if !ok {
		panic("sharedBufferPool.Get() returned a non-[]byte value")
	}

	return buf
}

func putBuffer(buffer *[]byte) {
	sharedBufferPool.Put(buffer)
}

func copyBuffer(dst io.Writer, src io.Reader) (written int64, err error) {
	buf := getBuffer()
	defer putBuffer(buf)

	for {
		nr, er := src.Read(*buf)
		if nr > 0 {
			nw, ew := dst.Write((*buf)[0:nr])
			if nw < 0 || nr < nw {
				nw = 0

				if ew == nil {
					ew = errors.New("invalid write result")
				}
			}

			written += int64(nw)

			if ew != nil {
				err = ew
				break
			}

			if nr != nw {
				err = io.ErrShortWrite
				break
			}
		}

		if er != nil {
			if er != io.EOF {
				err = er
			}
			break
		}
	}

	return written, err
}
