package providers

import (
	"context"
	"encoding/json"
	"net/http"
	"strings"

	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/settings"
	"golang.org/x/oauth2"
)

// https://openapi.logto.io/authentication
type logtoProvider struct {
	config   oauth2.Config
	endpoint string
}

func newLogtoProvider() provider.Interface {
	return &logtoProvider{
		config: oauth2.Config{
			Scopes: []string{"profile", "email", "phone", "name", "openid"},
		},
	}
}

func (p *logtoProvider) Init(opt provider.Oauth2Option) {
	p.config.ClientID = opt.ClientID
	p.config.ClientSecret = opt.ClientSecret
	p.config.RedirectURL = opt.RedirectURL
}

func (p *logtoProvider) NewAuthURL(ctx context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *logtoProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *logtoProvider) RefreshToken(ctx context.Context, token string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: token}).Token()
}

func (p *logtoProvider) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	tk, err := p.GetToken(ctx, code)
	if err != nil {
		return nil, err
	}
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, p.endpoint+"/oidc/me", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	var ui logtoUserInfo
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	un := ui.Username
	if un == "" {
		un = ui.Name
	}
	return &provider.UserInfo{
		ProviderUserID: ui.Sub,
		Username:       un,
	}, nil
}

type logtoUserInfo struct {
	Sub          string `json:"sub"`
	Username     string `json:"username"`
	PrimaryEmail string `json:"primaryEmail"`
	PrimaryPhone string `json:"primaryPhone"`
	Name         string `json:"name"`
	Email        string `json:"email"`
}

func (p *logtoProvider) RegistSetting(group string) {
	settings.NewStringSetting(
		group+"_endpoint", "", group,
		settings.WithAfterInitString(func(ss settings.StringSetting, s string) {
			s = strings.TrimSuffix(s, "/")
			s = strings.TrimSuffix(s, "/oidc")
			p.endpoint = s
			p.config.Endpoint = oauth2.Endpoint{
				AuthURL:  s + "/oidc/auth",
				TokenURL: s + "/oidc/token",
			}
		}),
		settings.WithAfterSetString(func(ss settings.StringSetting, s string) {
			s = strings.TrimSuffix(s, "/")
			s = strings.TrimSuffix(s, "/oidc")
			p.endpoint = s
			p.config.Endpoint = oauth2.Endpoint{
				AuthURL:  s + "/oidc/auth",
				TokenURL: s + "/oidc/token",
			}
		}),
	)
}

func (p *logtoProvider) Provider() provider.OAuth2Provider {
	return "logto"
}

func init() {
	RegisterProvider(newLogtoProvider())
}
