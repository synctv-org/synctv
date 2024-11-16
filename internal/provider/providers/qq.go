package providers

import (
	"context"
	"fmt"
	"net/http"
	"net/url"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/zijiren233/go-uhc"
	"golang.org/x/oauth2"
)

type QQProvider struct {
	config oauth2.Config
}

func newQQProvider() provider.Interface {
	return &QQProvider{
		config: oauth2.Config{
			Scopes: []string{"get_user_info"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://graph.qq.com/oauth2.0/authorize",
				TokenURL: "https://graph.qq.com/oauth2.0/token",
			},
		},
	}
}

func (p *QQProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *QQProvider) Provider() provider.OAuth2Provider {
	return "qq"
}

func (p *QQProvider) NewAuthURL(ctx context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *QQProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	params := url.Values{}
	params.Set("grant_type", "authorization_code")
	params.Set("code", code)
	params.Set("redirect_uri", p.config.RedirectURL)
	params.Set("client_id", p.config.ClientID)
	params.Set("client_secret", p.config.ClientSecret)
	params.Set("fmt", "json")
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("%s?%s", p.config.Endpoint.TokenURL, params.Encode()), nil)
	if err != nil {
		return nil, err
	}
	resp, err := uhc.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	tk := &oauth2.Token{}
	return tk, json.NewDecoder(resp.Body).Decode(tk)
}

func (p *QQProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	params := url.Values{}
	params.Set("grant_type", "refresh_token")
	params.Set("refresh_token", tk)
	params.Set("client_id", p.config.ClientID)
	params.Set("client_secret", p.config.ClientSecret)
	params.Set("fmt", "json")
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("%s?%s", p.config.Endpoint.TokenURL, params.Encode()), nil)
	if err != nil {
		return nil, err
	}
	resp, err := uhc.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	newTk := &oauth2.Token{}
	return newTk, json.NewDecoder(resp.Body).Decode(newTk)
}

func (p *QQProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := p.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://graph.qq.com/oauth2.0/me?access_token=%s&fmt=json", tk.AccessToken), nil)
	if err != nil {
		return nil, err
	}
	resp, err := uhc.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ume := qqProviderMe{}
	err = json.NewDecoder(resp.Body).Decode(&ume)
	if err != nil {
		return nil, err
	}
	req, err = http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://graph.qq.com/user/get_user_info?access_token=%s&oauth_consumer_key=%s&openid=%s&fmt=json", tk.AccessToken, p.config.ClientID, ume.Openid), nil)
	if err != nil {
		return nil, err
	}
	resp2, err := uhc.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp2.Body.Close()
	ui := qqUserInfo{}
	err = json.NewDecoder(resp2.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Nickname,
		ProviderUserID: ume.Openid,
	}, nil
}

//nolint:tagliatelle
type qqProviderMe struct {
	ClientID string `json:"client_id"`
	Openid   string `json:"openid"`
}

//nolint:tagliatelle
type qqUserInfo struct {
	Msg          string `json:"msg"`
	Nickname     string `json:"nickname"`
	Figureurl    string `json:"figureurl"`
	Figureurl1   string `json:"figureurl_1"`
	Figureurl2   string `json:"figureurl_2"`
	FigureurlQq1 string `json:"figureurl_qq_1"`
	FigureurlQq2 string `json:"figureurl_qq_2"`
	Gender       string `json:"gender"`
	Ret          int    `json:"ret"`
}

func init() {
	RegisterProvider(newQQProvider())
}
