package providers

import (
	"context"
	"fmt"
	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
	"net/http"
)

type FeishuProvider struct {
	config oauth2.Config
	SSOID  string
}

func (p *FeishuProvider) Init(c provider.Oauth2Option) {
	var SSOID = c.FeishuSSOID
	p.config.Scopes = []string{"profile"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/auth", SSOID),
		TokenURL: fmt.Sprintf("https://anycross.feishu.cn/sso/571495907/oauth2/token", SSOID),
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
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
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/userinfo", p.SSOID), nil)
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
