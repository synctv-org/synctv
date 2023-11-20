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

type FeishuProvider struct {
	config oauth2.Config
}

func (p *FeishuProvider) Init(c provider.Oauth2Option) {
	p.config.Scopes = []string{"profile"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  "飞书集成平台-身份集成-oauth2授权端点",
		TokenURL: "飞书集成平台-身份集成-oauth2Token端点",
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
	//请修改 GetUserInfo() 中的URL
}

func (p *FeishuProvider) Provider() provider.OAuth2Provider {
	return "feishuSSO"
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
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "飞书集成平台-身份集成-oauth2用户信息端点", nil)
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

func main() {
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: &FeishuProvider{}},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
