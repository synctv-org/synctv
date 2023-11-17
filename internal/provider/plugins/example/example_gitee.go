package main

import (
	"context"
	"encoding/json"
	"net/http"

	plugin "github.com/hashicorp/go-plugin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"golang.org/x/oauth2"
)

// go build -o gitee ./internal/provider/plugins/gitee.go
//
// mv gitee {data-dir}/plugins/oauth2/gitee
//
// config.yaml:
//
//	oauth2:
//		providers:
//			gitee:
//				client_id: xxx
//				client_secret: xxx
//				redirect_url: xxx
//		plugins:
//			- plugin_file: plugins/oauth2/gitee
//			  arges: []
type GiteeProvider struct {
	config oauth2.Config
}

func (p *GiteeProvider) Init(c provider.Oauth2Option) {
	p.config.Scopes = []string{"user_info"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "https://gitee.com/oauth/authorize",
		TokenURL: "https://gitee.com/oauth/token",
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *GiteeProvider) Provider() provider.OAuth2Provider {
	return "gitee"
}

func (p *GiteeProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *GiteeProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *GiteeProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *GiteeProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
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
	return &provider.UserInfo{
		Username:       ui.Login,
		ProviderUserID: ui.ID,
	}, nil
}

type giteeUserInfo struct {
	ID    uint64 `json:"id"`
	Login string `json:"login"`
}

func main() {
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: &GiteeProvider{}},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
