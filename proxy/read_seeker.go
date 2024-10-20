package proxy

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
)

var _ io.ReadSeekCloser = &HttpReadSeekCloser{}

type HttpReadSeekCloser struct {
	offset                int64
	maxOffset             int64
	url                   string
	contentLength         int64
	method                string
	headMethod            string
	headResp              *http.Response
	client                *http.Client
	headers               http.Header
	ctx                   context.Context
	allowedContentTypes   []string
	allowedStatusCodes    []int
	notAllowedStatusCodes []int
	length                int64
	currentResp           *http.Response
	currentRespOffset     int64
}

type HttpReadSeekerConf func(h *HttpReadSeekCloser)

func WithOffset(offset int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.offset = offset
	}
}

func WithMaxOffset(maxOffset int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.maxOffset = maxOffset
	}
}

func WithOffsetFromRange(rangeStr string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if r, err := ParseRange(rangeStr); err == nil {
			h.offset = r.Start
			h.maxOffset = r.End
		}
	}
}

func WithOffsetFromHeader(header http.Header) HttpReadSeekerConf {
	return WithOffsetFromRange(header.Get("Range"))
}

func WithHeaders(headers http.Header) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.headers = headers
		WithOffsetFromHeader(headers)(h)
	}
}

func WithHeaderMap(headers map[string]string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.headers = headerMapToHeader(headers)
		WithOffsetFromHeader(h.headers)(h)
	}
}

func WithClient(client *http.Client) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.client = client
	}
}

func WithMethod(method string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.method = method
	}
}

func WithHeadMethod(method string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.headMethod = method
	}
}

func WithContext(ctx context.Context) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.ctx = ctx
	}
}

func WithContentLength(contentLength int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if contentLength >= 0 {
			h.contentLength = contentLength
		}
	}
}

func AllowedContentTypes(types ...string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.allowedContentTypes = types
	}
}

func AllowedStatusCodes(codes ...int) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.allowedStatusCodes = codes
	}
}

func NotAllowedStatusCodes(codes ...int) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.notAllowedStatusCodes = codes
	}
}

func WithLength(length int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.length = length
	}
}

func NewHttpReadSeekCloser(url string, conf ...HttpReadSeekerConf) (*HttpReadSeekCloser, error) {
	rs := &HttpReadSeekCloser{
		offset:        0,
		url:           url,
		contentLength: -1,
		method:        http.MethodGet,
		headMethod:    http.MethodHead,
		length:        64 * 1024, // Default length
		maxOffset:     -1,        // Default to no max offset
		headers:       make(http.Header),
	}
	for _, c := range conf {
		c(rs)
	}
	rs.fix()
	if err := rs.fetchContentLength(); err != nil {
		return nil, err
	}
	if rs.offset < 0 {
		rs.offset = rs.contentLength - rs.maxOffset
		rs.maxOffset = rs.contentLength
	}
	if rs.maxOffset < 0 {
		rs.maxOffset = rs.contentLength
	}
	return rs, nil
}

func (h *HttpReadSeekCloser) fix() *HttpReadSeekCloser {
	if h.method == "" {
		h.method = http.MethodGet
	}
	if h.ctx == nil {
		h.ctx = context.Background()
	}
	if h.client == nil {
		h.client = http.DefaultClient
	}
	if len(h.notAllowedStatusCodes) == 0 {
		h.notAllowedStatusCodes = []int{http.StatusNotFound}
	}
	return h
}

func (h *HttpReadSeekCloser) Read(p []byte) (n int, err error) {
	for n < len(p) {
		if h.maxOffset >= 0 && h.offset >= h.maxOffset {
			return n, io.EOF
		}

		if h.currentResp == nil || h.offset >= h.currentRespOffset+h.length {
			if err := h.FetchNextChunk(); err != nil {
				return n, err
			}
		}

		// Calculate the maximum number of bytes we can read
		maxRead := len(p[n:])
		if h.maxOffset >= 0 {
			remainingBytes := h.maxOffset - h.offset
			if int64(maxRead) > remainingBytes {
				maxRead = int(remainingBytes)
			}
		}

		// Read only up to maxRead bytes
		readN, err := h.currentResp.Body.Read(p[n : n+maxRead])
		n += readN
		h.offset += int64(readN)

		if h.maxOffset >= 0 && h.offset >= h.maxOffset {
			return n, io.EOF
		}

		if err == io.EOF {
			h.closeCurrentResp()
			if n < len(p) {
				continue
			}
			return n, nil
		}
		if err != nil {
			return n, err
		}
	}

	return n, nil
}

func (h *HttpReadSeekCloser) FetchNextChunk() error {
	h.closeCurrentResp()

	req, err := h.createRequest()
	if err != nil {
		return err
	}

	resp, err := h.client.Do(req)
	if err != nil {
		return err
	}

	if err := h.checkResponse(resp); err != nil {
		resp.Body.Close()
		return err
	}

	h.currentResp = resp
	h.currentRespOffset = h.offset
	return nil
}

func (h *HttpReadSeekCloser) createRequest() (*http.Request, error) {
	req, err := h.createRequestWithoutRange()
	if err != nil {
		return nil, err
	}
	endByte := h.offset + h.length - 1
	if h.maxOffset >= 0 && endByte > h.maxOffset {
		endByte = h.maxOffset
	}
	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", h.offset, endByte))
	return req, nil
}

func (h *HttpReadSeekCloser) createRequestWithoutRange() (*http.Request, error) {
	req, err := http.NewRequestWithContext(h.ctx, h.method, h.url, nil)
	if err != nil {
		return nil, err
	}
	req.Header = h.headers.Clone()
	req.Header.Set("Range", "bytes=0-")
	return req, nil
}

func (h *HttpReadSeekCloser) checkResponse(resp *http.Response) error {
	if err := h.checkStatusCode(resp.StatusCode); err != nil {
		return err
	}
	return h.checkContentType(resp.Header.Get("Content-Type"))
}

func (h *HttpReadSeekCloser) closeCurrentResp() {
	if h.currentResp != nil {
		h.currentResp.Body.Close()
		h.currentResp = nil
	}
}

func (h *HttpReadSeekCloser) checkContentType(ct string) error {
	if len(h.allowedContentTypes) != 0 {
		if ct == "" || slices.Index(h.allowedContentTypes, ct) == -1 {
			return fmt.Errorf("content type `%s` not allowed", ct)
		}
	}
	return nil
}

func (h *HttpReadSeekCloser) checkStatusCode(code int) error {
	if len(h.allowedStatusCodes) != 0 {
		if slices.Index(h.allowedStatusCodes, code) == -1 {
			return fmt.Errorf("status code `%d` not allowed", code)
		}
	}
	if len(h.notAllowedStatusCodes) != 0 {
		if slices.Index(h.notAllowedStatusCodes, code) != -1 {
			return fmt.Errorf("status code `%d` not allowed", code)
		}
	}
	return nil
}

func (h *HttpReadSeekCloser) Seek(offset int64, whence int) (int64, error) {
	newOffset, err := h.calculateNewOffset(offset, whence)
	if err != nil {
		return 0, err
	}

	if h.maxOffset >= 0 && newOffset > h.maxOffset {
		return 0, fmt.Errorf("seek position %d is beyond maxOffset %d", newOffset, h.maxOffset)
	}

	if newOffset != h.offset {
		h.closeCurrentResp()
		h.offset = newOffset
	}

	return h.offset, nil
}

func (h *HttpReadSeekCloser) calculateNewOffset(offset int64, whence int) (int64, error) {
	switch whence {
	case io.SeekStart:
		return offset, nil
	case io.SeekCurrent:
		return h.offset + offset, nil
	case io.SeekEnd:
		return h.contentLength - offset, nil
	default:
		return 0, errors.New("invalid whence")
	}
}

func (h *HttpReadSeekCloser) fetchContentLength() error {
	if h.contentLength >= 0 {
		return nil
	}
	req, err := h.createRequestWithoutRange()
	if err != nil {
		return err
	}
	req.Method = h.headMethod

	resp, err := h.client.Do(req)
	if err != nil {
		return err
	}
	defer resp.Body.Close()

	if err := h.checkResponse(resp); err != nil {
		return err
	}

	if resp.ContentLength < 0 {
		return errors.New("invalid content length")
	}

	h.contentLength = resp.ContentLength
	h.headResp = resp
	return nil
}

func (h *HttpReadSeekCloser) Close() error {
	if h.currentResp != nil {
		return h.currentResp.Body.Close()
	}
	return nil
}

func (h *HttpReadSeekCloser) Offset() int64 {
	return h.offset
}

func (h *HttpReadSeekCloser) ContentLength() int64 {
	return h.contentLength
}

func (h *HttpReadSeekCloser) ContentType() string {
	return h.headResp.Header.Get("Content-Type")
}

func (h *HttpReadSeekCloser) AcceptRanges() string {
	return h.headResp.Header.Get("Accept-Ranges")
}

func (h *HttpReadSeekCloser) ContentRange() string {
	return h.headResp.Header.Get("Content-Range")
}
