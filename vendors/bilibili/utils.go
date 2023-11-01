package bilibili

import (
	"errors"
	"regexp"

	"github.com/synctv-org/synctv/utils"
)

var (
	BVRegex  = regexp.MustCompile(`^(?:https://www\.bilibili\.com/video/)?((?:bv|bV|Bv|BV)\w+)(?:[/\?].*)?$`)
	ARegex   = regexp.MustCompile(`^(?:https://www\.bilibili\.com/video/)?(?:av|aV|Av|AV)(\d+)(?:[/\?].*)?$`)
	SSRegex  = regexp.MustCompile(`^(?:https://www\.bilibili\.com/bangumi/play/)?(?:ss|sS|Ss|SS)(\d+)(?:\?.*)?$`)
	EPRegex  = regexp.MustCompile(`^(?:https://www\.bilibili\.com/bangumi/play/)?(?:ep|eP|Ep|EP)(\d+)(?:\?.*)?$`)
	B23Regex = regexp.MustCompile(`^(https://)?b23\.tv/(\w+)$`)
)

func Match(url string) (t string, id string, err error) {
	if B23Regex.MatchString(url) {
		resp, err := utils.NoRedirectHttpClient().Get(url)
		if err != nil {
			return "", "", err
		}
		resp.Body.Close()
		return Match(resp.Header.Get("Location"))
	}
	if m := BVRegex.FindStringSubmatch(url); m != nil {
		return "bv", m[1], nil
	}
	if m := ARegex.FindStringSubmatch(url); m != nil {
		return "av", m[1], nil
	}
	if m := SSRegex.FindStringSubmatch(url); m != nil {
		return "ss", m[1], nil
	}
	if m := EPRegex.FindStringSubmatch(url); m != nil {
		return "ep", m[1], nil
	}
	return "", "", errors.New("match failed")
}
