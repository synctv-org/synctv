package proxy

import "io"

type BufferedReadSeeker struct {
	r                 io.ReadSeeker
	buffer            []byte
	readIdx, writeIdx int
}

func NewBufferedReadSeeker(r io.ReadSeeker, bufSize int) *BufferedReadSeeker {
	if bufSize == 0 {
		bufSize = 64 * 1024
	}
	return &BufferedReadSeeker{r: r, buffer: make([]byte, bufSize)}
}

func (b *BufferedReadSeeker) Reset(r io.ReadSeeker) {
	b.r = r
	b.readIdx, b.writeIdx = 0, 0
}

func (b *BufferedReadSeeker) Read(p []byte) (n int, err error) {
	if len(p) == 0 {
		return n, err
	}

	if b.readIdx == b.writeIdx {
		if len(p) >= len(b.buffer) {
			n, err = b.r.Read(p)
			return n, err
		}
		b.readIdx, b.writeIdx = 0, 0

		n, err = b.r.Read(b.buffer)
		if n == 0 {
			return n, err
		}

		b.writeIdx += n
	}

	n = copy(p, b.buffer[b.readIdx:b.writeIdx])
	b.readIdx += n

	return n, err
}

func (b *BufferedReadSeeker) Seek(offset int64, whence int) (int64, error) {
	n, err := b.r.Seek(offset, whence)

	b.Reset(b.r)

	return n, err
}

func (b *BufferedReadSeeker) ReadAt(p []byte, off int64) (int, error) {
	_, err := b.Seek(off, io.SeekStart)
	if err != nil {
		return 0, err
	}

	return b.Read(p)
}
