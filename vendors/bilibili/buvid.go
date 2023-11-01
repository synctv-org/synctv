package bilibili

import (
	"errors"
	"net/http"
	"sync"
	"time"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/utils"
)

var (
	bLock           sync.RWMutex
	b3, b4          string
	bLastUpdateTime time.Time
)

func getBuvidCookies() ([]*http.Cookie, error) {
	b3, b4, err := getBuvid()
	if err != nil {
		return nil, err
	}
	return []*http.Cookie{
		{
			Name:  "buvid3",
			Value: b3,
		},
		{
			Name:  "buvid4",
			Value: b4,
		},
	}, nil
}

func getBuvid() (string, string, error) {
	bLock.RLock()
	if time.Since(bLastUpdateTime) < time.Hour {
		bLock.RUnlock()
		return b3, b4, nil
	}
	bLock.RUnlock()
	bLock.Lock()
	defer bLock.Unlock()
	if time.Since(bLastUpdateTime) < time.Hour {
		return b3, b4, nil
	}
	var err error
	b3, b4, err = newBuvid()
	if err != nil {
		return "", "", err
	}
	bLastUpdateTime = time.Now()
	return b3, b4, nil
}

type buvid struct {
	Code int `json:"code"`
	Data struct {
		B3 string `json:"b_3"`
		B4 string `json:"b_4"`
	} `json:"data"`
	Message string `json:"message"`
}

func newBuvid() (string, string, error) {
	r, err := http.NewRequest(http.MethodGet, "https://api.bilibili.com/x/frontend/finger/spi", nil)
	if err != nil {
		return "", "", err
	}
	r.Header.Set("User-Agent", utils.UA)
	r.Header.Set("Referer", "https://www.bilibili.com")
	resp, err := http.DefaultClient.Do(r)
	if err != nil {
		return "", "", err
	}
	defer resp.Body.Close()
	var b buvid
	err = json.NewDecoder(resp.Body).Decode(&b)
	if err != nil {
		return "", "", err
	}
	if b.Code != 0 {
		return "", "", errors.New(b.Message)
	}
	return b.Data.B3, b.Data.B4, nil
}
