package providers

import (
	"context"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
	"net/http"
)

type FeishuProvider struct {
	config      oauth2.Config
	UserInfoURL string
}

func (p *FeishuProvider) Init(c provider.Oauth2Option) {
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "授权端点",
		TokenURL: "Token 端点",
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
	p.UserInfoURL = "用户信息端点"
}

func (p *FeishuProvider) Provider() provider.OAuth2Provider {
	return "feishu"
}

func (p *FeishuProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *FeishuProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *FeishuProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *FeishuProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, p.UserInfoURL, nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := feishuUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.UserID,
	}, nil
}

type feishuUserInfo struct {
	UserID string `json:"user_id"`
	Name   string `json:"name"`
}

func init() {
	RegisterProvider(new(FeishuProvider))
}
