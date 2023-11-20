package main

import (
	"context"
	"encoding/json"
	"fmt"
	plugin "github.com/hashicorp/go-plugin"
	"github.com/synctv-org/synctv/internal/provider"
	"github.com/synctv-org/synctv/internal/provider/plugins"
	"golang.org/x/oauth2"
	"net/http"
	"os"
)

// Mac/Linux:
// go build -o feishuSSO ./internal/provider/plugins/example/example_feishuSSO/example_feishuSSO.go
//
// Windows:
// go build -o feishuSSO.exe ./internal/provider/plugins/example/example_feishuSSO/example_feishuSSO.go
//
// mv gitee {data-dir}/plugins/oauth2/feishuSSO
//
// 飞书SSO登录插件设置信息：飞书集成平台-身份集成-应用单点登录-你创建的SSO服务
//
// config.yaml:
//
//	oauth2:
//		providers:
//			feishuSSO:
//				client_id: "App ID"
//				client_secret: "App Secret"
//				redirect_url: "登录回调地址"
//		plugins:
//			- plugin_file: plugins/oauth2/feishuSSO
//			  arges:
//				- "OAuth2.0协议端点中的一串数字"
type FeishuProvider struct {
	config oauth2.Config
	ssoid  string // Your SSO Application ID in Feishu Anycross
}

func (p *FeishuProvider) Init(c provider.Oauth2Option) {
	p.config.Scopes = []string{"profile"}
	p.config.Endpoint = oauth2.Endpoint{
		AuthURL:  fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/auth", p.ssoid),  // 认证端点
		TokenURL: fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/token", p.ssoid), //Token 端点
	}
	p.config.ClientID = c.ClientID
	p.config.ClientSecret = c.ClientSecret
	p.config.RedirectURL = c.RedirectURL
}

func (p *FeishuProvider) Provider() provider.OAuth2Provider {
	return "feishuSSO" //插件名
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
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, fmt.Sprintf("https://anycross.feishu.cn/sso/%s/oauth2/userinfo", p.ssoid), nil) // 身份端点
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
	UserID string `json:"user_id"` // 飞书UserID （企业内唯一ID）
	Name   string `json:"name"`    // 飞书姓名（会作为SyncTV登录用户名）
}

func main() {
	args := os.Args
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: &FeishuProvider{ssoid: args[1]}},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
