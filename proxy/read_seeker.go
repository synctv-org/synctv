package proxy

import (
	"bytes"
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"strconv"
)

type HttpReadSeeker struct {
	offset        int64
	url           string
	contentLength int64
	method        string
	body          io.Reader
	client        *http.Client
	headers       map[string]string
	ctx           context.Context
}

type HttpReadSeekerConf func(h *HttpReadSeeker)

func WithHeaders(headers map[string]string) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		h.headers = headers
	}
}

func WithAppendHeaders(headers map[string]string) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		if h.headers == nil {
			h.headers = make(map[string]string)
		}
		for k, v := range headers {
			h.headers[k] = v
		}
	}
}

func WithClient(client *http.Client) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		h.client = client
	}
}

func WithMethod(method string) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		h.method = method
	}
}

func WithContext(ctx context.Context) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		h.ctx = ctx
	}
}

func WithBody(body []byte) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		if len(body) != 0 {
			h.body = bytes.NewReader(body)
		}
	}
}

func WithContentLength(contentLength int64) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		if contentLength >= 0 {
			h.contentLength = contentLength
		}
	}
}

func WithStartOffset(offset int64) HttpReadSeekerConf {
	return func(h *HttpReadSeeker) {
		if offset >= 0 {
			h.offset = offset
		}
	}
}

func NewHttpReadSeeker(url string, conf ...HttpReadSeekerConf) *HttpReadSeeker {
	rs := &HttpReadSeeker{
		offset:        0,
		url:           url,
		contentLength: -1,
		method:        http.MethodGet,
	}
	for _, c := range conf {
		c(rs)
	}
	rs.fix()
	return rs
}

func NewBufferedHttpReadSeeker(bufSize int, url string, conf ...HttpReadSeekerConf) *BufferedReadSeeker {
	if bufSize == 0 {
		bufSize = 64 * 1024
	}
	return &BufferedReadSeeker{r: NewHttpReadSeeker(url, conf...), buffer: make([]byte, bufSize)}
}

func (h *HttpReadSeeker) fix() *HttpReadSeeker {
	if h.method == "" {
		h.method = http.MethodGet
	}
	if h.ctx == nil {
		h.ctx = context.Background()
	}
	if h.client == nil {
		h.client = http.DefaultClient
	}
	return h
}

func (h *HttpReadSeeker) Read(p []byte) (n int, err error) {
	req, err := http.NewRequestWithContext(h.ctx, h.method, h.url, h.body)
	if err != nil {
		return 0, err
	}
	for k, v := range h.headers {
		req.Header.Set(k, v)
	}
	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", h.offset, h.offset+int64(len(p))-1))
	resp, err := h.client.Do(req)
	if err != nil {
		return 0, err
	}
	defer resp.Body.Close()
	n, err = io.ReadFull(resp.Body, p)
	h.offset += int64(n)
	return n, err
}

func (h *HttpReadSeeker) Seek(offset int64, whence int) (int64, error) {
	switch whence {
	case io.SeekStart:
		h.offset = offset
	case io.SeekCurrent:
		h.offset += offset
	case io.SeekEnd:
		if h.contentLength < 0 {
			req, err := http.NewRequestWithContext(h.ctx, http.MethodHead, h.url, nil)
			if err != nil {
				return 0, err
			}
			for k, v := range h.headers {
				req.Header.Set(k, v)
			}
			resp, err := h.client.Do(req)
			if err != nil {
				return 0, err
			}
			defer resp.Body.Close()

			h.contentLength, err = strconv.ParseInt(resp.Header.Get("Content-Length"), 10, 64)
			if err != nil {
				return 0, err
			}
			if h.contentLength < 0 {
				return 0, errors.New("content length error")
			}
		}
		h.offset = h.contentLength - offset
	default:
		return 0, errors.New("whence value error")
	}
	return h.offset, nil
}
