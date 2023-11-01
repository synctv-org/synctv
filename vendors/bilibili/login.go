package bilibili

import (
	"fmt"
	"net/http"
	"net/url"
	"strings"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/utils"
)

type RQCode struct {
	URL string `json:"url"`
	Key string `json:"key"`
}

func NewQRCode() (*RQCode, error) {
	req, err := http.NewRequest(http.MethodGet, "https://passport.bilibili.com/x/passport-login/web/qrcode/generate", nil)
	if err != nil {
		return nil, err
	}
	resp, err := http.DefaultClient.Do(req)
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
func LoginWithQRCode(key string) (*http.Cookie, error) {
	req, err := http.NewRequest(http.MethodGet, fmt.Sprintf("https://passport.bilibili.com/x/passport-login/web/qrcode/auth?oauthKey=%s", key), nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", utils.UA)
	resp, err := http.DefaultClient.Do(req)
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

type CaptchaResp struct {
	Token     string `json:"token"`
	Gt        string `json:"gt"`
	Challenge string `json:"challenge"`
}

func NewCaptcha() (*CaptchaResp, error) {
	req, err := http.NewRequest(http.MethodGet, "https://passport.bilibili.com/x/passport-login/captcha", nil)
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", utils.UA)
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	var captcha captcha
	err = json.NewDecoder(resp.Body).Decode(&captcha)
	if err != nil {
		return nil, err
	}
	return &CaptchaResp{
		Token:     captcha.Data.Token,
		Gt:        captcha.Data.Geetest.Gt,
		Challenge: captcha.Data.Geetest.Challenge,
	}, nil
}

type captcha struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	TTL     int    `json:"ttl"`
	Data    struct {
		Type    string `json:"type"`
		Token   string `json:"token"`
		Geetest struct {
			Challenge string `json:"challenge"`
			Gt        string `json:"gt"`
		} `json:"geetest"`
		Tencent struct {
			Appid string `json:"appid"`
		} `json:"tencent"`
	} `json:"data"`
}

type sms struct {
	Code    int    `json:"code"`
	Message string `json:"message"`
	Data    struct {
		CaptchaKey string `json:"captcha_key"`
	} `json:"data"`
}

func NewSMS(tel, token, challenge, validate string) (captchaKey string, err error) {
	buvid3, err := newBuvid3()
	if err != nil {
		return "", err
	}
	data := url.Values{}
	data.Set("cid", "86")
	data.Set("tel", tel)
	data.Set("source", "main-fe-header")
	data.Set("token", token)
	data.Set("challenge", challenge)
	data.Set("validate", validate)
	data.Set("seccode", fmt.Sprintf("%s|jordan", validate))

	req, err := http.NewRequest(http.MethodPost, "https://passport.bilibili.com/x/passport-login/web/sms/send", strings.NewReader(data.Encode()))
	if err != nil {
		return "", err
	}
	req.Header.Set("User-Agent", utils.UA)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	req.AddCookie(buvid3)

	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	var sms sms
	err = json.NewDecoder(resp.Body).Decode(&sms)
	if err != nil {
		return "", err
	}
	return sms.Data.CaptchaKey, nil
}

func LoginWithSMS(tel, code, captchaKey string) (*http.Cookie, error) {
	data := url.Values{}
	data.Set("cid", "86")
	data.Set("tel", tel)
	data.Set("code", code)
	data.Set("source", "main-fe-header")
	data.Set("captcha_key", captchaKey)

	req, err := http.NewRequest(http.MethodPost, "https://passport.bilibili.com/x/passport-login/web/login/sms", strings.NewReader(data.Encode()))
	if err != nil {
		return nil, err
	}
	req.Header.Set("User-Agent", utils.UA)
	req.Header.Set("Content-Type", "application/x-www-form-urlencoded")
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	for _, cookie := range resp.Cookies() {
		if cookie.Name == "SESSDATA" {
			return cookie, nil
		}
	}
	return nil, fmt.Errorf("no SESSDATA cookie")
}
