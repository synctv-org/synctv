package main

import (
	"context"
	"encoding/json"
	"fmt"
	"os"

	plugin "github.com/hashicorp/go-plugin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"golang.org/x/oauth2"
	"net/http"
)

// Linux/Mac/Windows:
// CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build ./internal/provider/plugins/example/example_feishu-sso/example_feishu-sso.go
// CGO_ENABLED=0 GOOS=dawin GOARCH=amd64 go build ./internal/provider/plugins/example/example_feishu-sso/example_feishu-sso.go
// CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build ./internal/provider/plugins/example/example_feishu-sso/example_feishu-sso.go
//
// mv gitee {data-dir}/plugins/oauth2/FeishuSSO
//
// 飞书集成平台单点应用：https://anycross.feishu.cn/console/identity/sso-app-manager/
//
// config.yaml:
//
// oauth2_plugins:
//   - plugin_file: plugins/oauth2/FeishuSSO
//     args: ["单点应用ID（一串纯数字）"]
type FeishuSSOProvider struct {
	config oauth2.Config
	ssoid  string
}

func newFeishuSSOProvider(ssoid string) provider.ProviderInterface {
	return &FeishuSSOProvider{
		config: oauth2.Config{
			Scopes: []string{"profile"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/auth", ssoid),  // 授权码（authorization_code）获取接口
				TokenURL: fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/token", ssoid), // 获取访问令牌（access_token）
			},
		},
		ssoid: ssoid,
	}
}

func (p *FeishuSSOProvider) Init(c provider.Oauth2Option) {
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *FeishuSSOProvider) Provider() provider.OAuth2Provider {
	return "feishu-sso" //插件名
}

func (p *FeishuSSOProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *FeishuSSOProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *FeishuSSOProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *FeishuSSOProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/userinfo", p.ssoid), nil) // 身份端点
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := FeishuSSOUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.ID,
	}, nil
}

type FeishuSSOUserInfo struct {
	ID   string `json:"sub"`  // 组织内SSO应用账号ID
	Name string `json:"name"` // 飞书账号昵称
}

func main() {
	args := os.Args
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: newFeishuSSOProvider(args[1])},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
