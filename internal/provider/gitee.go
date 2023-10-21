package provider

import (
	"context"
	"net/http"

	json "github.com/json-iterator/go"
	"golang.org/x/oauth2"
)

type GiteeProvider struct {
	config oauth2.Config
}

func (p *GiteeProvider) Init(ClientID, ClientSecret string, options ...Oauth2Option) {
	p.config.ClientID = ClientID
	p.config.ClientSecret = ClientSecret
	p.config.Scopes = []string{"user_info"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "https://gitee.com/oauth/authorize",
		TokenURL: "https://gitee.com/oauth/token",
	}
	for _, o := range options {
		o(&p.config)
	}
}

func (p *GiteeProvider) Provider() OAuth2Provider {
	return "gitee"
}

func (p *GiteeProvider) NewConfig(options ...Oauth2Option) *oauth2.Config {
	c := p.config
	for _, o := range options {
		o(&c)
	}
	if c.RedirectURL == "" {
		panic("gitee oauth2 redirect url is empty")
	}
	return &c
}

func (p *GiteeProvider) GetUserInfo(ctx context.Context, config *oauth2.Config, code string) (*UserInfo, error) {
	oauth2Token, err := config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := config.Client(ctx, oauth2Token)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://gitee.com/api/v5/user", nil)
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := giteeUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &UserInfo{
		Username:       ui.Login,
		ProviderUserID: ui.ID,
	}, nil
}

type giteeUserInfo struct {
	ID    uint   `json:"id"`
	Login string `json:"login"`
}

func init() {
	registerProvider(new(GiteeProvider))
}
