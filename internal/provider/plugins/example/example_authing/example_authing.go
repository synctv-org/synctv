package main

import (
	"context"
	"encoding/json"
	"fmt"
	"net/http"
	"os"

	plugin "github.com/hashicorp/go-plugin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"golang.org/x/oauth2"
)

// Linux/Mac/Windows:
// CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build
// ./internal/provider/plugins/example/example_authing/example_authing.go CGO_ENABLED=0 GOOS=dawin
// GOARCH=amd64 go build ./internal/provider/plugins/example/example_authing/example_authing.go
// CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build
// ./internal/provider/plugins/example/example_authing/example_authing.go
//
// mv gitee {data-dir}/plugins/oauth2/authing
//
// Authing：https://console.authing.cn/
//
// config.yaml:
//
// oauth2_plugins:
//   - plugin_file: plugins/oauth2/authing
//     args: ["认证配置-认证地址（只需要你自定义的那个部分）"]
type AuthingProvider struct {
	config oauth2.Config
}

func newAuthingProvider(authURL string) provider.Interface {
	return &AuthingProvider{
		config: oauth2.Config{
			Scopes: []string{"profile"},
			Endpoint: oauth2.Endpoint{
				AuthURL: fmt.Sprintf(
					"https://%s.authing.cn/oauth/auth",
					authURL,
				), // 授权码（authorization_code）获取接口
				TokenURL: fmt.Sprintf("https://%s.authing.cn/oauth/token", authURL), // Token端点
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
	return "authing" // 插件名
}

func (p *AuthingProvider) NewAuthURL(_ context.Context, state string) (string, error) {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline), nil
}

func (p *AuthingProvider) GetUserInfo(
	ctx context.Context,
	code string,
) (*provider.UserInfo, error) {
	tk, err := p.config.Exchange(ctx, code)
	if err != nil {
		return nil, err
	}
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(
		ctx,
		http.MethodGet,
		"https://core.authing.cn/oauth/me",
		nil,
	) // 身份端点
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
		ProviderUserID: ui.UnionID,
	}, nil
}

type AuthingUserInfo struct {
	UnionID string `json:"sub"`  // Authing用户ID
	Name    string `json:"name"` // Authing用户名
}

func main() {
	args := os.Args
	pluginMap := map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: newAuthingProvider(args[1])},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
