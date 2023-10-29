package bilibili

import (
	"fmt"
	"net/http"

	json "github.com/json-iterator/go"
)

type RQCode struct {
	URL string `json:"url"`
	Key string `json:"key"`
}

func NewQRCode() (*RQCode, error) {
	resp, err := http.Get("https://passport.bilibili.com/x/passport-login/web/qrcode/generate")
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	qr := qrcodeResp{}
	err = json.NewDecoder(resp.Body).Decode(&qr)
	if err != nil {
		return nil, err
	}
	// TODO: error message
	return &RQCode{
		URL: qr.Data.URL,
		Key: qr.Data.QrcodeKey,
	}, nil
}

// return SESSDATA cookie
func Login(key string) (*http.Cookie, error) {
	resp, err := http.Get(fmt.Sprintf("https://passport.bilibili.com/x/passport-login/web/qrcode/poll?qrcode_key=%s", key))
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	for _, cookie := range resp.Cookies() {
		if cookie.Name == "SESSDATA" {
			return cookie, nil
		}
	}
	return nil, fmt.Errorf("no SESSDATA cookie")
}
