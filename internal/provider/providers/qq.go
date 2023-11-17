package providers

import (
	"context"
	"fmt"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
)

type QQProvider struct {
	config oauth2.Config
}

func (p *QQProvider) Init(c provider.Oauth2Option) {
	p.config.Scopes = []string{"get_user_info"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "https://graph.qq.com/oauth2.0/authorize",
		TokenURL: "https://graph.qq.com/oauth2.0/token?grant_type=authorization_code",
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *QQProvider) Provider() provider.OAuth2Provider {
	return "qq"
}

func (p *QQProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *QQProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *QQProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *QQProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://graph.qq.com/oauth2.0/me?access_token=%s&fmt=json", tk.AccessToken), nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := qqProviderUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Openid,
		ProviderUserID: ui.Openid,
	}, nil
}

func init() {
	RegisterProvider(new(QQProvider))
}

type qqProviderUserInfo struct {
	ClientID string `json:"client_id"`
	Openid   string `json:"openid"`
}
