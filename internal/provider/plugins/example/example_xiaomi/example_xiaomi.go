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

// Linux/Mac/Windows:
// CGO_ENABLED=0 GOOS=linux GOARCH=amd64 go build ./internal/provider/plugins/example/example_xiaomi/example_xiaomi.go
// CGO_ENABLED=0 GOOS=dawin GOARCH=amd64 go build ./internal/provider/plugins/example/example_xiaomi/example_xiaomi.go
// CGO_ENABLED=0 GOOS=windows GOARCH=amd64 go build ./internal/provider/plugins/example/example_xiaomi/example_xiaomi.go
//
// mv gitee {data-dir}/plugins/oauth2/xiaomi
//
// 小米开放平台：https://dev.mi.com/
//
// config.yaml:
//
// oauth2_plugins:
//   - plugin_file: plugins/oauth2/xiaomi
//     args: []
type XiaomiProvider struct {
	config oauth2.Config
}

func newXiaomiProvider() provider.ProviderInterface {
	return &XiaomiProvider{
		config: oauth2.Config{
			Scopes: []string{"profile"},
			Endpoint: oauth2.Endpoint{
				AuthURL:  "https://account.xiaomi.com/oauth2/authorize", // 认证端点
				TokenURL: "https://account.xiaomi.com/oauth2/token",     //Token 端点
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
	return "xiaomi" //插件名
}

func (p *XiaomiProvider) NewAuthURL(state string) string {
	return p.config.AuthCodeURL(state, oauth2.AccessTypeOnline)
}

func (p *XiaomiProvider) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	return p.config.Exchange(ctx, code)
}

func (p *XiaomiProvider) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	return p.config.TokenSource(ctx, &oauth2.Token{RefreshToken: tk}).Token()
}

func (p *XiaomiProvider) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
	client := p.config.Client(ctx, tk)
	req, err := http.NewRequestWithContext(ctx, http.MethodGet, "https://open.account.xiaomi.com/user/profile", nil) // 身份端点
	if err != nil {
		return nil, err
	}
	resp, err := client.Do(req)
	if err != nil {
		return nil, err
	}
	defer resp.Body.Close()
	ui := XiaomiUserInfo{}
	err = json.NewDecoder(resp.Body).Decode(&ui)
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       ui.Name,
		ProviderUserID: ui.UnionId,
	}, nil
}

type XiaomiUserInfo struct {
	UnionId string `json:"unionId"`    // 飞书UserID （企业内唯一ID）
	Name    string `json:"miliaoNick"` // 飞书姓名（会作为SyncTV登录用户名）
}

func main() {
	var pluginMap = map[string]plugin.Plugin{
		"Provider": &plugins.ProviderPlugin{Impl: newXiaomiProvider()},
	}
	plugin.Serve(&plugin.ServeConfig{
		HandshakeConfig: plugins.HandshakeConfig,
		Plugins:         pluginMap,
		GRPCServer:      plugin.DefaultGRPCServer,
	})
}
