package proxy

import (
	"context"
	"errors"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"strings"
)

var (
	_ io.ReadSeekCloser = (*HttpReadSeekCloser)(nil)
	_ Proxy             = (*HttpReadSeekCloser)(nil)
)

type HttpReadSeekCloser struct {
	ctx                   context.Context
	headHeaders           http.Header
	currentResp           *http.Response
	headers               http.Header
	client                *http.Client
	contentType           string
	method                string
	headMethod            string
	url                   string
	allowedContentTypes   []string
	notAllowedStatusCodes []int
	allowedStatusCodes    []int
	offset                int64
	contentLength         int64
	length                int64
	currentRespMaxOffset  int64
}

type HttpReadSeekerConf func(h *HttpReadSeekCloser)

func WithHeaders(headers http.Header) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if headers != nil {
			h.headers = headers.Clone()
		}
	}
}

func WithHeadersMap(headers map[string]string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		for k, v := range headers {
			h.headers.Set(k, v)
		}
	}
}

func WithClient(client *http.Client) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if client != nil {
			h.client = client
		}
	}
}

func WithMethod(method string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if method != "" {
			h.method = method
		}
	}
}

func WithHeadMethod(method string) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if method != "" {
			h.headMethod = method
		}
	}
}

func WithContext(ctx context.Context) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if ctx != nil {
			h.ctx = ctx
		}
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
		if len(types) > 0 {
			h.allowedContentTypes = slices.Clone(types)
		}
	}
}

func AllowedStatusCodes(codes ...int) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if len(codes) > 0 {
			h.allowedStatusCodes = slices.Clone(codes)
		}
	}
}

func NotAllowedStatusCodes(codes ...int) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if len(codes) > 0 {
			h.notAllowedStatusCodes = slices.Clone(codes)
		}
	}
}

func WithLength(length int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if length > 0 {
			h.length = length
		}
	}
}

func NewHttpReadSeekCloser(url string, conf ...HttpReadSeekerConf) *HttpReadSeekCloser {
	rs := &HttpReadSeekCloser{
		url:           url,
		contentLength: -1,
		method:        http.MethodGet,
		headMethod:    http.MethodHead,
		length:        64 * 1024, // Default length
		headers:       make(http.Header),
		ctx:           context.Background(),
		client:        http.DefaultClient,
	}

	for _, c := range conf {
		if c != nil {
			c(rs)
		}
	}

	rs.fix()

	return rs
}

func (h *HttpReadSeekCloser) fix() *HttpReadSeekCloser {
	if h.method == "" {
		h.method = http.MethodGet
	}
	if h.headMethod == "" {
		h.headMethod = http.MethodHead
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
	if h.length <= 0 {
		h.length = 64 * 1024
	}
	if h.headers == nil {
		h.headers = make(http.Header)
	}
	return h
}

func (h *HttpReadSeekCloser) Read(p []byte) (n int, err error) {
	for n < len(p) {
		if h.currentResp == nil || h.offset > h.currentRespMaxOffset {
			if err := h.FetchNextChunk(); err != nil {
				if err == io.EOF {
					return n, io.EOF
				}
				return 0, fmt.Errorf("fetch next chunk: %w", err)
			}
		}

		readN, err := h.currentResp.Body.Read(p[n:])
		if readN > 0 {
			n += readN
			h.offset += int64(readN)
		}

		if err == io.EOF {
			h.closeCurrentResp()
			if n < len(p) {
				continue
			}
			break
		}
		if err != nil {
			if n > 0 {
				return n, nil
			}
			return 0, fmt.Errorf("read response body: %w", err)
		}
	}

	return n, nil
}

func (h *HttpReadSeekCloser) FetchNextChunk() error {
	h.closeCurrentResp()

	if h.contentLength > 0 && h.offset >= h.contentLength {
		return io.EOF
	}

	req, err := h.createRequest()
	if err != nil {
		return fmt.Errorf("create request: %w", err)
	}

	resp, err := h.client.Do(req)
	if err != nil {
		return fmt.Errorf("do request: %w", err)
	}

	if resp.StatusCode != http.StatusPartialContent && resp.StatusCode != http.StatusOK {
		resp.Body.Close()
		return fmt.Errorf("status code: %d", resp.StatusCode)
	}

	if err := h.checkResponse(resp); err != nil {
		resp.Body.Close()
		return fmt.Errorf("check response: %w", err)
	}

	switch resp.StatusCode {
	case http.StatusPartialContent:
		contentTotalLength, err := ParseContentRangeTotalLength(resp.Header.Get("Content-Range"))
		if err == nil && contentTotalLength > 0 {
			h.contentLength = contentTotalLength
		}
		_, end, err := ParseContentRangeStartAndEnd(resp.Header.Get("Content-Range"))
		if err == nil {
			h.currentRespMaxOffset = end
		}
	case http.StatusOK:
		h.contentLength = resp.ContentLength
		h.currentRespMaxOffset = h.contentLength - 1
	}

	h.contentType = resp.Header.Get("Content-Type")

	h.currentResp = resp
	return nil
}

func (h *HttpReadSeekCloser) createRequest() (*http.Request, error) {
	req, err := h.createRequestWithoutRange()
	if err != nil {
		return nil, err
	}

	end := h.offset + h.length - 1
	if h.contentLength > 0 && end > h.contentLength-1 {
		end = h.contentLength - 1
	}

	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", h.offset, end))
	return req, nil
}

func (h *HttpReadSeekCloser) createRequestWithoutRange() (*http.Request, error) {
	req, err := http.NewRequestWithContext(h.ctx, h.method, h.url, nil)
	if err != nil {
		return nil, err
	}
	req.Header = h.headers.Clone()
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
		return nil
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
		return 0, fmt.Errorf("calculate new offset: %w", err)
	}

	if newOffset < 0 {
		return 0, fmt.Errorf("negative offset: %d", newOffset)
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
		if h.contentLength < 0 {
			if err := h.fetchContentLength(); err != nil {
				return 0, fmt.Errorf("fetch content length: %w", err)
			}
		}
		return h.contentLength - offset, nil
	default:
		return 0, fmt.Errorf("invalid whence: %d", whence)
	}
}

func (h *HttpReadSeekCloser) fetchContentLength() error {
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

	h.contentType = resp.Header.Get("Content-Type")

	h.contentLength = resp.ContentLength
	h.headHeaders = resp.Header.Clone()
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

func (h *HttpReadSeekCloser) ContentType() (string, error) {
	if h.contentType != "" {
		return h.contentType, nil
	}
	return "", fmt.Errorf("content type not available")
}

func (h *HttpReadSeekCloser) ContentTotalLength() (int64, error) {
	if h.contentLength > 0 {
		return h.contentLength, nil
	}
	return 0, fmt.Errorf("content total length not available")
}

func ParseContentRangeStartAndEnd(contentRange string) (int64, int64, error) {
	if contentRange == "" {
		return 0, 0, fmt.Errorf("empty content range")
	}

	if !strings.HasPrefix(contentRange, "bytes ") {
		return 0, 0, fmt.Errorf("invalid content range format: %s", contentRange)
	}

	parts := strings.Split(strings.TrimPrefix(contentRange, "bytes "), "/")
	if len(parts) != 2 {
		return 0, 0, fmt.Errorf("invalid content range parts: %s", contentRange)
	}

	rangeParts := strings.Split(strings.TrimSpace(parts[0]), "-")
	if len(rangeParts) != 2 {
		return 0, 0, fmt.Errorf("invalid content range range parts: %s", contentRange)
	}

	start, err := strconv.ParseInt(strings.TrimSpace(rangeParts[0]), 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("invalid content range start: %w", err)
	}

	end, err := strconv.ParseInt(strings.TrimSpace(rangeParts[1]), 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("invalid content range end: %w", err)
	}

	if start < 0 || end < 0 {
		return 0, 0, fmt.Errorf("negative content range bounds: start=%d, end=%d", start, end)
	}

	if start > end {
		return 0, 0, fmt.Errorf("invalid content range bounds: start=%d > end=%d", start, end)
	}

	return start, end, nil
}

// ParseContentRangeTotalLength parses a Content-Range header value and returns the total length
func ParseContentRangeTotalLength(contentRange string) (int64, error) {
	if contentRange == "" {
		return 0, fmt.Errorf("empty content range")
	}

	if !strings.HasPrefix(contentRange, "bytes ") {
		return 0, fmt.Errorf("invalid content range format: %s", contentRange)
	}

	parts := strings.Split(strings.TrimPrefix(contentRange, "bytes "), "/")
	if len(parts) != 2 {
		return 0, fmt.Errorf("invalid content range parts: %s", contentRange)
	}

	if parts[1] == "" || parts[1] == "*" {
		return -1, nil
	}

	length, err := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid content range length: %w", err)
	}

	if length < 0 {
		return 0, fmt.Errorf("negative content range length: %d", length)
	}

	return length, nil
}
