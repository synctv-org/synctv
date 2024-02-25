package providers

import (
	"context"
	"fmt"
	"net/http"

	json "github.com/json-iterator/go"
	"github.com/synctv-org/synctv/internal/provider"
	"golang.org/x/oauth2"
)

type XiaomiProvider struct {
	config oauth2.Config
}

func newXiaomiProvider() provider.ProviderInterface {
	return &XiaomiProvider{
		config: oauth2.Config{
			Scopes: []string{"profile"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://account.xiaomi.com/oauth2/authorize",
				TokenURL: "https://account.xiaomi.com/oauth2/token",
			},
		},
	}
}

func (p *XiaomiProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *XiaomiProvider) Provider() provider.OAuth2Provider {
	return "xiaomi"
}

func (p *XiaomiProvider) NewAuthURL(ctx context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *XiaomiProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *XiaomiProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *XiaomiProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := p.config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://open.account.xiaomi.com/user/profile?clientId=%s&token=%s", p.config.ClientID, tk.AccessToken), nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := xiaomiUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Data.Name,
		ProviderUserID: ui.Data.UnionId,
	}, nil
}

type xiaomiUserInfo struct {
	Data struct {
		UnionId string `json:"unionId"`
		Name    string `json:"miliaoNick"`
	} `json:"data"`
}

func init() {
	RegisterProvider(newXiaomiProvider())
}
