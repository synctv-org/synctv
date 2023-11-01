package bilibili

import (
	"crypto/md5"
	"encoding/hex"
	"net/http"
	"net/url"
	"sort"
	"strconv"
	"strings"
	"time"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/utils"
	refreshcache "github.com/synctv-org/synctv/utils/refreshCache"
)

var (
	mixinKeyEncTab = []int{
		46, 47, 18, 2, 53, 8, 23, 32, 15, 50, 10, 31, 58, 3, 45, 35, 27, 43, 5, 49,
		33, 9, 42, 19, 29, 28, 14, 39, 12, 38, 41, 13, 37, 48, 7, 16, 24, 55, 40,
		61, 26, 17, 0, 1, 60, 51, 30, 4, 22, 25, 54, 21, 56, 59, 6, 63, 57, 62, 11,
		36, 20, 34, 44, 52,
	}
	wbiCache = refreshcache.NewRefreshCache[key](func() (key, error) {
		imgKey, subKey, err := getWbiKeys()
		if err != nil {
			return key{}, err
		}
		return key{imgKey, subKey}, nil
	}, time.Minute*10)
)

type key struct {
	imgKey, subKey string
}

func signAndGenerateURL(urlStr string) (string, error) {
	urlObj, err := url.Parse(urlStr)
	if err != nil {
		return "", err
	}
	key, err := wbiCache.Get()
	if err != nil {
		return "", err
	}
	query := urlObj.Query()
	params := map[string]string{}
	for k, v := range query {
		params[k] = v[0]
	}
	newParams := encWbi(params, key.imgKey, key.subKey)
	for k, v := range newParams {
		query.Set(k, v)
	}
	urlObj.RawQuery = query.Encode()
	return urlObj.String(), nil
}

func encWbi(params map[string]string, imgKey, subKey string) map[string]string {
	mixinKey := getMixinKey(imgKey + subKey)
	currTime := strconv.FormatInt(time.Now().Unix(), 10)
	params["wts"] = currTime

	keys := make([]string, 0, len(params))
	for k := range params {
		keys = append(keys, k)
	}
	sort.Strings(keys)

	for k, v := range params {
		v = sanitizeString(v)
		params[k] = v
	}

	query := url.Values{}
	for _, k := range keys {
		query.Set(k, params[k])
	}
	queryStr := query.Encode()

	hash := md5.Sum([]byte(queryStr + mixinKey))
	params["w_rid"] = hex.EncodeToString(hash[:])
	return params
}

func getMixinKey(orig string) string {
	var str strings.Builder
	for _, v := range mixinKeyEncTab {
		if v < len(orig) {
			str.WriteByte(orig[v])
		}
	}
	return str.String()[:32]
}

func sanitizeString(s string) string {
	unwantedChars := []string{"!", "'", "(", ")", "*"}
	for _, char := range unwantedChars {
		s = strings.ReplaceAll(s, char, "")
	}
	return s
}

func getWbiKeys() (string, string, error) {
	req, err := http.NewRequest(http.MethodGet, "https://api.bilibili.com/x/web-interface/nav", nil)
	if err != nil {
		return "", "", err
	}
	req.Header.Set("User-Agent", utils.UA)
	req.Header.Set("Referer", "https://www.bilibili.com")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", "", err
	}
	defer resp.Body.Close()
	info := Nav{}
	err = json.NewDecoder(resp.Body).Decode(&info)
	if err != nil {
		return "", "", err
	}

	imgKey := strings.Split(strings.Split(info.Data.WbiImg.ImgURL, "/")[len(strings.Split(info.Data.WbiImg.ImgURL, "/"))-1], ".")[0]
	subKey := strings.Split(strings.Split(info.Data.WbiImg.SubURL, "/")[len(strings.Split(info.Data.WbiImg.SubURL, "/"))-1], ".")[0]
	return imgKey, subKey, nil
}
