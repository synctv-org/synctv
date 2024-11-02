package proxy

import (
	"context"
	"fmt"
	"io"
	"net/http"
	"slices"
	"strconv"
	"strings"

	"github.com/synctv-org/synctv/utils"
)

var (
	_ io.ReadSeekCloser = (*HttpReadSeekCloser)(nil)
	_ Proxy             = (*HttpReadSeekCloser)(nil)
)

type HttpReadSeekCloser struct {
	ctx                               context.Context
	headHeaders                       http.Header
	currentResp                       *http.Response
	headers                           http.Header
	client                            *http.Client
	contentType                       string
	method                            string
	headMethod                        string
	url                               string
	allowedContentTypes               []string
	offset                            int64
	contentTotalLength                int64
	perLength                         int64
	currentRespMaxOffset              int64
	notSupportRange                   bool
	notSupportSeekWhenNotSupportRange bool
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

func WithContentTotalLength(contentTotalLength int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if contentTotalLength >= 0 {
			h.contentTotalLength = contentTotalLength
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

// sets the per length of the request
func WithPerLength(length int64) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		if length > 0 {
			h.perLength = length
		}
	}
}

func WithForceNotSupportRange(notSupportRange bool) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.notSupportRange = notSupportRange
	}
}

func WithNotSupportSeekWhenNotSupportRange(notSupportSeekWhenNotSupportRange bool) HttpReadSeekerConf {
	return func(h *HttpReadSeekCloser) {
		h.notSupportSeekWhenNotSupportRange = notSupportSeekWhenNotSupportRange
	}
}

func NewHttpReadSeekCloser(url string, conf ...HttpReadSeekerConf) *HttpReadSeekCloser {
	rs := &HttpReadSeekCloser{
		url:                url,
		contentTotalLength: -1,
		method:             http.MethodGet,
		headMethod:         http.MethodHead,
		perLength:          1024 * 1024 * 16,
		headers:            make(http.Header),
		client:             http.DefaultClient,
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
	if h.perLength <= 0 {
		h.perLength = 1024 * 1024
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
				return 0, fmt.Errorf("failed to fetch next chunk: %w", err)
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
			return 0, fmt.Errorf("error reading response body: %w", err)
		}
	}

	return n, nil
}

func (h *HttpReadSeekCloser) FetchNextChunk() error {
	h.closeCurrentResp()

	if h.contentTotalLength > 0 && h.offset >= h.contentTotalLength {
		return io.EOF
	}

	req, err := h.createRequest()
	if err != nil {
		return fmt.Errorf("failed to create request: %w", err)
	}

	resp, err := h.client.Do(req)
	if err != nil {
		return fmt.Errorf("failed to execute request: %w", err)
	}

	if resp.StatusCode != http.StatusPartialContent &&
		resp.StatusCode != http.StatusOK &&
		resp.StatusCode != http.StatusRequestedRangeNotSatisfiable {
		resp.Body.Close()
		return fmt.Errorf("unexpected status code: %d", resp.StatusCode)
	}

	if resp.StatusCode == http.StatusRequestedRangeNotSatisfiable {
		contentTotalLength, err := ParseContentRangeTotalLength(resp.Header.Get("Content-Range"))
		if err == nil && contentTotalLength > 0 {
			h.contentTotalLength = contentTotalLength
		}
		resp.Body.Close()
		return fmt.Errorf("requested range not satisfiable, content total length: %d, offset: %d", h.contentTotalLength, h.offset)
	}

	if err := h.checkContentType(resp.Header.Get("Content-Type")); err != nil {
		resp.Body.Close()
		return fmt.Errorf("response validation failed: %w", err)
	}

	h.contentType = resp.Header.Get("Content-Type")

	if resp.StatusCode == http.StatusOK {
		if h.offset > 0 {
			if h.notSupportSeekWhenNotSupportRange {
				return fmt.Errorf("not support seek when not support range")
			}
			if _, err := io.CopyN(io.Discard, resp.Body, h.offset); err != nil {
				resp.Body.Close()
				return fmt.Errorf("failed to discard bytes: %w", err)
			}
		}

		h.notSupportRange = true
		h.contentTotalLength = resp.ContentLength
		h.currentRespMaxOffset = h.contentTotalLength - 1
		h.currentResp = resp
		return nil
	}

	contentTotalLength, err := ParseContentRangeTotalLength(resp.Header.Get("Content-Range"))
	if err == nil && contentTotalLength > 0 {
		h.contentTotalLength = contentTotalLength
	}
	start, end, err := ParseContentRangeStartAndEnd(resp.Header.Get("Content-Range"))
	if err == nil {
		if end != -1 {
			h.currentRespMaxOffset = end
		}
		if h.offset != start {
			return fmt.Errorf("offset mismatch, expected: %d, got: %d", start, h.offset)
		}
	}

	h.currentResp = resp
	return nil
}

func (h *HttpReadSeekCloser) createRequest() (*http.Request, error) {
	if h.notSupportRange {
		if h.contentTotalLength != -1 {
			h.currentRespMaxOffset = h.contentTotalLength - 1
		}
		return h.createRequestWithoutRange()
	}

	req, err := h.createRequestWithoutRange()
	if err != nil {
		return nil, err
	}

	end := h.offset + h.perLength - 1
	if h.contentTotalLength > 0 && end > h.contentTotalLength-1 {
		end = h.contentTotalLength - 1
	}

	h.currentRespMaxOffset = end

	req.Header.Set("Range", fmt.Sprintf("bytes=%d-%d", h.offset, end))
	return req, nil
}

func (h *HttpReadSeekCloser) createRequestWithoutRange() (*http.Request, error) {
	req, err := http.NewRequestWithContext(h.ctx, h.method, h.url, nil)
	if err != nil {
		return nil, fmt.Errorf("failed to create request: %w", err)
	}
	req.Header = h.headers.Clone()
	req.Header.Del("Range")
	if req.Header.Get("User-Agent") == "" {
		req.Header.Set("User-Agent", utils.UA)
	}
	return req, nil
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
			return fmt.Errorf("content type '%s' is not in the list of allowed content types: %v", ct, h.allowedContentTypes)
		}
	}
	return nil
}

func (h *HttpReadSeekCloser) Seek(offset int64, whence int) (int64, error) {
	newOffset, err := h.calculateNewOffset(offset, whence)
	if err != nil {
		h.closeCurrentResp()
		return 0, fmt.Errorf("failed to calculate new offset: %w", err)
	}

	if newOffset < 0 {
		h.closeCurrentResp()
		return 0, fmt.Errorf("cannot seek to negative offset: %d", newOffset)
	}

	if newOffset != h.offset {
		h.closeCurrentResp()
		if h.notSupportRange && h.notSupportSeekWhenNotSupportRange {
			return 0, fmt.Errorf("seek is not supported when not support range")
		}
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
		if h.contentTotalLength < 0 {
			if err := h.fetchContentLength(); err != nil {
				return 0, fmt.Errorf("failed to fetch content length: %w", err)
			}
		}
		return h.contentTotalLength - offset, nil
	default:
		return 0, fmt.Errorf("invalid seek whence value: %d", whence)
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
		return fmt.Errorf("failed to execute HEAD request: %w", err)
	}
	defer resp.Body.Close()

	if resp.StatusCode != http.StatusOK {
		return fmt.Errorf("unexpected status code in HEAD request: %d", resp.StatusCode)
	}

	if err := h.checkContentType(resp.Header.Get("Content-Type")); err != nil {
		return fmt.Errorf("HEAD response validation failed: %w", err)
	}

	if resp.ContentLength < 0 {
		return fmt.Errorf("server returned invalid content length: %d", resp.ContentLength)
	}

	h.contentType = resp.Header.Get("Content-Type")

	h.contentTotalLength = resp.ContentLength
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
	return h.contentTotalLength
}

func (h *HttpReadSeekCloser) ContentType() (string, error) {
	if h.contentType != "" {
		return h.contentType, nil
	}
	return "", fmt.Errorf("content type is not available - no successful response received yet")
}

func (h *HttpReadSeekCloser) ContentTotalLength() (int64, error) {
	if h.contentTotalLength > 0 {
		return h.contentTotalLength, nil
	}
	return 0, fmt.Errorf("content total length is not available - no successful response received yet")
}

func ParseContentRangeStartAndEnd(contentRange string) (int64, int64, error) {
	if contentRange == "" {
		return 0, 0, fmt.Errorf("Content-Range header is empty")
	}

	if !strings.HasPrefix(contentRange, "bytes ") {
		return 0, 0, fmt.Errorf("invalid Content-Range header format (expected 'bytes ' prefix): %s", contentRange)
	}

	parts := strings.Split(strings.TrimPrefix(contentRange, "bytes "), "/")
	if len(parts) != 2 {
		return 0, 0, fmt.Errorf("invalid Content-Range header format (expected 2 parts separated by '/'): %s", contentRange)
	}

	rangeParts := strings.Split(strings.TrimSpace(parts[0]), "-")
	if len(rangeParts) != 2 {
		return 0, 0, fmt.Errorf("invalid Content-Range range format (expected start-end): %s", contentRange)
	}

	start, err := strconv.ParseInt(strings.TrimSpace(rangeParts[0]), 10, 64)
	if err != nil {
		return 0, 0, fmt.Errorf("invalid Content-Range start value '%s': %w", rangeParts[0], err)
	}

	rangeParts[1] = strings.TrimSpace(rangeParts[1])
	var end int64
	if rangeParts[1] == "" || rangeParts[1] == "*" {
		end = -1
	} else {
		end, err = strconv.ParseInt(rangeParts[1], 10, 64)
		if err != nil {
			return 0, 0, fmt.Errorf("invalid Content-Range end value '%s': %w", rangeParts[1], err)
		}
	}

	return start, end, nil
}

// ParseContentRangeTotalLength parses a Content-Range header value and returns the total length
func ParseContentRangeTotalLength(contentRange string) (int64, error) {
	if contentRange == "" {
		return 0, fmt.Errorf("Content-Range header is empty")
	}

	if !strings.HasPrefix(contentRange, "bytes ") {
		return 0, fmt.Errorf("invalid Content-Range header format (expected 'bytes ' prefix): %s", contentRange)
	}

	parts := strings.Split(strings.TrimPrefix(contentRange, "bytes "), "/")
	if len(parts) != 2 {
		return 0, fmt.Errorf("invalid Content-Range header format (expected 2 parts separated by '/'): %s", contentRange)
	}

	if parts[1] == "" || parts[1] == "*" {
		return -1, nil
	}

	length, err := strconv.ParseInt(strings.TrimSpace(parts[1]), 10, 64)
	if err != nil {
		return 0, fmt.Errorf("invalid Content-Range total length value '%s': %w", parts[1], err)
	}

	if length < 0 {
		return 0, fmt.Errorf("Content-Range total length cannot be negative: %d", length)
	}

	return length, nil
}
