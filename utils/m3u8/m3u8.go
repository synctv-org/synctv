package m3u8

import (
	"bufio"
	"fmt"
	"net/url"
	"strings"
)

func GetM3u8AllSegments(m3u8Str string, baseUrl string) ([]string, error) {
	var segments []string
	err := RangeM3u8SegmentsWithBaseUrl(m3u8Str, baseUrl, func(segmentUrl string) error {
		segments = append(segments, segmentUrl)
		return nil
	})
	if err != nil {
		return nil, err
	}
	return segments, nil
}

func RangeM3u8Segments(m3u8Str string, callback func(segmentUrl string) error) error {
	scanner := bufio.NewScanner(strings.NewReader(m3u8Str))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" && !strings.HasPrefix(line, "#") {
			if err := callback(line); err != nil {
				return err
			}
		}
	}
	if err := scanner.Err(); err != nil {
		return fmt.Errorf("scan m3u8 error: %w", err)
	}
	return nil
}

func RangeM3u8SegmentsWithBaseUrl(m3u8Str string, baseUrl string, callback func(segmentUrl string) error) error {
	baseUrlParsed, err := url.Parse(baseUrl)
	if err != nil {
		return fmt.Errorf("parse base url error: %w", err)
	}
	return RangeM3u8Segments(m3u8Str, func(segmentUrl string) error {
		if !strings.HasPrefix(segmentUrl, "http://") && !strings.HasPrefix(segmentUrl, "https://") {
			segmentUrlParsed, err := url.Parse(segmentUrl)
			if err != nil {
				return fmt.Errorf("parse segment url error: %w", err)
			}
			segmentUrl = baseUrlParsed.ResolveReference(segmentUrlParsed).String()
		}
		return callback(segmentUrl)
	})
}

func ReplaceM3u8Segments(m3u8Str string, callback func(segmentUrl string) (string, error)) (string, error) {
	var result strings.Builder
	scanner := bufio.NewScanner(strings.NewReader(m3u8Str))
	for scanner.Scan() {
		line := strings.TrimSpace(scanner.Text())
		if line != "" && !strings.HasPrefix(line, "#") {
			newSegment, err := callback(line)
			if err != nil {
				return "", fmt.Errorf("callback error: %w", err)
			}
			result.WriteString(newSegment)
		} else {
			result.WriteString(line)
		}
		result.WriteString("\n")
	}
	if err := scanner.Err(); err != nil {
		return "", fmt.Errorf("scan m3u8 error: %w", err)
	}
	return result.String(), nil
}

func ReplaceM3u8SegmentsWithBaseUrl(m3u8Str string, baseUrl string, callback func(segmentUrl string) (string, error)) (string, error) {
	baseUrlParsed, err := url.Parse(baseUrl)
	if err != nil {
		return "", fmt.Errorf("parse base url error: %w", err)
	}
	return ReplaceM3u8Segments(m3u8Str, func(segmentUrl string) (string, error) {
		if !strings.HasPrefix(segmentUrl, "http://") && !strings.HasPrefix(segmentUrl, "https://") {
			segmentUrlParsed, err := url.Parse(segmentUrl)
			if err != nil {
				return "", fmt.Errorf("parse segment url error: %w", err)
			}
			segmentUrl = baseUrlParsed.ResolveReference(segmentUrlParsed).String()
		}
		return callback(segmentUrl)
	})
}
