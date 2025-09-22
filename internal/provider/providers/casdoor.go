package providers

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"net/url"

	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/settings"
	"golang.org/x/oauth2"
)

// https://door.casdoor.com/.well-known/openid-configuration
type casdoorProvider struct {
	config   oauth2.Config
	endpoint string
}

func newCasdoorProvider() provider.Interface {
	return &casdoorProvider{
		config: oauth2.Config{
			Scopes: []string{"profile", "email", "phone", "name", "openid"},
		},
	}
}

func (p *casdoorProvider) Init(opt provider.Oauth2Option) {
	p.config.ClientID = opt.ClientID
	p.config.ClientSecret = opt.ClientSecret
	p.config.RedirectURL = opt.RedirectURL
}

func (p *casdoorProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *casdoorProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *casdoorProvider) RefreshToken(ctx context.Context, token string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: token}).Token()
}

func (p *casdoorProvider) GetUserInfo(
	ctx context.Context,
	code string,
) (*provider.UserInfo, error) {
	tk, err := p.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}

	client := p.config.Client(ctx, tk)

	req, err := http.NewRequestWithContext(ctx, http.MethodGet, p.endpoint+"/api/userinfo", nil)
	if err != nil {
		return nil, err
	}

	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()

	var ui casdoorUserInfo

	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}

	un := ui.PreferredUsername
	if un == "" {
		un = ui.Name
	}

	return &provider.UserInfo{
		ProviderUserID: ui.Sub,
		Username:       un,
	}, nil
}

type casdoorUserInfo struct {
	Sub               string `json:"sub"`
	PreferredUsername string `json:"preferred_username"`
	Name              string `json:"name"`
	Email             string `json:"email"`
	Phone             string `json:"phone"`
}

func (p *casdoorProvider) RegistSetting(group string) {
	settings.NewStringSetting(
		group+"_endpoint", "", group,
		settings.WithAfterInitString(func(_ settings.StringSetting, s string) {
			p.endpoint = s
			p.config.Endpoint = oauth2.Endpoint{
				AuthURL:  s + "/login/oauth/authorize",
				TokenURL: s + "/api/login/oauth/access_token",
			}
		}),
		settings.WithBeforeSetString(func(_ settings.StringSetting, s string) (string, error) {
			u, err := url.Parse(s)
			if err != nil {
				return "", err
			}

			return fmt.Sprintf("%s://%s", u.Scheme, u.Host), nil
		}),
		settings.WithAfterSetString(func(_ settings.StringSetting, s string) {
			p.endpoint = s
			p.config.Endpoint = oauth2.Endpoint{
				AuthURL:  s + "/login/oauth/authorize",
				TokenURL: s + "/api/login/oauth/access_token",
			}
		}),
	)
}

func (p *casdoorProvider) Provider() provider.OAuth2Provider {
	return "casdoor"
}

func init() {
	RegisterProvider(newCasdoorProvider())
}
