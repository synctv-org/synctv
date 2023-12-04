package main

import (
	"context"
	"encoding/json"
	plugin "github.com/hashicorp/go-plugin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"golang.org/x/oauth2"
	"net/http"
)

// Linux/Mac/Windows:
// CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build ./internal/provider/plugins/example/example_Authing/example_Authing.go
// CGO_ENABLED=0 GOOS=dawin GOARCH=amd64 go build ./internal/provider/plugins/example/example_Authing/example_Authing.go
// CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build ./internal/provider/plugins/example/example_Authing/example_Authing.go
//
// mv gitee {data-dir}/plugins/oauth2/Authing
//
// Authing：https://console.authing.cn/
//
// config.yaml:
//
// oauth2_plugins:
//   - plugin_file: plugins/oauth2/Authing
//     args: []
type AuthingProvider struct {
	config oauth2.Config
}

func newAuthingProvider() provider.ProviderInterface {
	return &AuthingProvider{
		config: oauth2.Config{
			Scopes: []string{"profile"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://synctv-login.authing.cn/oauth/auth",  // 授权码（authorization_code）获取接口
				TokenURL: "https://synctv-login.authing.cn/oauth/token", // 获取访问令牌（access_token）
			},
		},
	}
}

func (p *AuthingProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *AuthingProvider) Provider() provider.OAuth2Provider {
	return "Authing" //插件名
}

func (p *AuthingProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *AuthingProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *AuthingProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *AuthingProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://core.authing.cn/oauth/me", nil) // 身份端点
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := AuthingUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.UnionId,
	}, nil
}

type AuthingUserInfo struct {
	UnionId string `json:"sub"`  // Authing用户ID
	Name    string `json:"name"` // Authing用户名
}

func main() {
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: newAuthingProvider()},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
