package aggregations

import (
	"context"
	"fmt"
	"net/http"
	"net/url"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
)

var _ provider.AggregationProviderInterface = (*Rainbow)(nil)

const DefaultRainbowApi = "https://u.cccyun.cc"

type Rainbow struct {
	api string
}

func (r *Rainbow) SetAPI(api string) {
	r.api = api
}

func (r *Rainbow) Provider() provider.OAuth2Provider {
	return "rainbow"
}

func (r *Rainbow) Providers() []provider.OAuth2Provider {
	return []provider.OAuth2Provider{
		"qq",
		"wx",
		"alipay",
		"baidu",
		"microsoft",
	}
}

type rainbowGenericProvider struct {
	parent *Rainbow
	t      string
	conf   provider.Oauth2Option
}

func (r *Rainbow) newGenericProvider(t string) provider.ProviderInterface {
	return &rainbowGenericProvider{
		parent: r,
		t:      t,
	}
}

func (p *rainbowGenericProvider) Init(c provider.Oauth2Option) {
	p.conf = c
}

func (p *rainbowGenericProvider) Provider() provider.OAuth2Provider {
	switch p.t {
	case "wx":
		return "wechat"
	default:
		return p.t
	}
}

func (p *rainbowGenericProvider) NewAuthURL(ctx context.Context, state string) (string, error) {
	result, err := url.JoinPath(p.parent.api, "/connect.php")
	if err != nil {
		return "", err
	}
	u, err := url.Parse(result)
	if err != nil {
		return "", err
	}
	query := url.Values{}
	query.Set("act", "login")
	query.Set("appid", p.conf.ClientID)
	query.Set("appkey", p.conf.ClientSecret)
	query.Set("type", p.t)
	query.Set("redirect_uri", p.conf.RedirectURL)
	query.Set("state", state)
	u.RawQuery = query.Encode()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return "", err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return "", err
	}
	defer resp.Body.Close()
	data := rainbowNewAuthURLResp{}
	err = json.NewDecoder(resp.Body).Decode(&data)
	if err != nil {
		return "", err
	}
	if data.Code != 0 {
		return "", fmt.Errorf("error code: %d, msg: %s", data.ErrCode, data.Msg)
	}
	return data.URL, nil
}

type rainbowNewAuthURLResp struct {
	Code    int    `json:"code"`
	ErrCode int    `json:"errcode"`
	Msg     string `json:"msg"`
	Type    string `json:"type"`
	URL     string `json:"url"`
}

func (p *rainbowGenericProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	result, err := url.JoinPath(p.parent.api, "/connect.php")
	if err != nil {
		return nil, err
	}
	u, err := url.Parse(result)
	if err != nil {
		return nil, err
	}
	query := url.Values{}
	query.Set("act", "callback")
	query.Set("appid", p.conf.ClientID)
	query.Set("appkey", p.conf.ClientSecret)
	query.Set("type", p.t)
	query.Set("code", code)
	u.RawQuery = query.Encode()

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, u.String(), nil)
	if err != nil {
		return nil, err
	}
	resp, err := http.DefaultClient.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	data := rainbowUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&data)
	if err != nil {
		return nil, err
	}
	if data.Code != 0 {
		return nil, fmt.Errorf("error code: %d, msg: %s", data.ErrCode, data.Msg)
	}
	return &provider.UserInfo{
		Username:       data.Nickname,
		ProviderUserID: data.SocialUID,
	}, nil
}

type rainbowUserInfo struct {
	Code      int    `json:"code"`
	ErrCode   int    `json:"errcode"`
	Msg       string `json:"msg"`
	Type      string `json:"type"`
	SocialUID string `json:"social_uid"`
	Nickname  string `json:"nickname"`
}

func (r *Rainbow) ExtractProvider(p provider.OAuth2Provider) (provider.ProviderInterface, error) {
	switch p {
	case "qq", "wx", "alipay", "baidu", "microsoft":
		return r.newGenericProvider(p), nil
	default:
		return nil, fmt.Errorf("provider %s not supported", p)
	}
}

func init() {
	addAggregation(new(Rainbow))
}
