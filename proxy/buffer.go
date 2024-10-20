package proxy

import (
	"io"
)

type BufferedReadSeeker struct {
	r                 io.ReadSeeker
	buffer            []byte
	readIdx, writeIdx int
}

func NewBufferedReadSeeker(r io.ReadSeeker, bufSize int) *BufferedReadSeeker {
	if bufSize <= 0 {
		bufSize = 64 * 1024
	}
	return &BufferedReadSeeker{r: r, buffer: make([]byte, bufSize)}
}

func NewBufferedReadSeekerWithBuffer(r io.ReadSeeker, buffer []byte) *BufferedReadSeeker {
	return &BufferedReadSeeker{r: r, buffer: buffer}
}

func (b *BufferedReadSeeker) Reset(r io.ReadSeeker) {
	b.r = r
	b.readIdx, b.writeIdx = 0, 0
}

func (b *BufferedReadSeeker) Read(p []byte) (n int, err error) {
	if len(p) == 0 {
		return 0, nil
	}

	if b.readIdx == b.writeIdx {
		if len(p) >= len(b.buffer) {
			return b.r.Read(p)
		}
		b.readIdx, b.writeIdx = 0, 0

		b.writeIdx, err = b.r.Read(b.buffer)
		if b.writeIdx == 0 {
			return 0, err
		}
	}

	n = copy(p, b.buffer[b.readIdx:b.writeIdx])
	b.readIdx += n

	return n, nil
}

func (b *BufferedReadSeeker) Seek(offset int64, whence int) (int64, error) {
	n, err := b.r.Seek(offset, whence)
	if err == nil {
		b.readIdx, b.writeIdx = 0, 0
	}
	return n, err
}

func (b *BufferedReadSeeker) ReadAt(p []byte, off int64) (int, error) {
	if _, err := b.Seek(off, io.SeekStart); err != nil {
		return 0, err
	}
	return io.ReadFull(b, p)
}
