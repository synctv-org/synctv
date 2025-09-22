package providers

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
)

// https://pan.baidu.com/union/apply
type BaiduProvider struct {
	config oauth2.Config
}

func newBaiduProvider() provider.Interface {
	return &BaiduProvider{
		config: oauth2.Config{
			Scopes: []string{"basic"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://openapi.baidu.com/oauth/2.0/authorize",
				TokenURL: "https://openapi.baidu.com/oauth/2.0/token",
			},
		},
	}
}

func (p *BaiduProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *BaiduProvider) Provider() provider.OAuth2Provider {
	return "baidu"
}

func (p *BaiduProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *BaiduProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *BaiduProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *BaiduProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := p.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}

	client := p.config.Client(ctx, tk)

	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodGet,
		"https://openapi.baidu.com/rest/2.0/passport/users/getLoggedInUser?access_token="+tk.AccessToken,
		nil,
	)
	if err != nil {
		return nil, err
	}

	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	ui := baiduProviderUserInfo{}

	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}

	return &provider.UserInfo{
		Username:       ui.Uname,
		ProviderUserID: ui.Openid,
	}, nil
}

type baiduProviderUserInfo struct {
	Uname  string `json:"uname"`
	Openid string `json:"openid"`
}

func init() {
	RegisterProvider(newBaiduProvider())
}
