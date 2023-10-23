package provider

import (
	"context"
	"fmt"
	"os/exec"
	"time"

	"github.com/hashicorp/go-hclog"
	plugin "github.com/hashicorp/go-plugin"
	log "github.com/sirupsen/logrus"
	sysnotify "github.com/synctv-org/synctv/internal/sysNotify"
	providerpb "github.com/synctv-org/synctv/proto/provider"
	"golang.org/x/oauth2"
	"google.golang.org/grpc"
)

type ProviderPlugin struct {
	plugin.Plugin
	Impl ProviderInterface
}

func (p *ProviderPlugin) GRPCServer(broker *plugin.GRPCBroker, s *grpc.Server) error {
	providerpb.RegisterOauth2PluginServer(s, &GRPCServer{Impl: p.Impl})
	return nil
}

func (p *ProviderPlugin) GRPCClient(ctx context.Context, broker *plugin.GRPCBroker, c *grpc.ClientConn) (interface{}, error) {
	return &GRPCClient{client: providerpb.NewOauth2PluginClient(c)}, nil
}

type GRPCServer struct {
	providerpb.UnimplementedOauth2PluginServer
	Impl ProviderInterface
}

func (s *GRPCServer) Init(ctx context.Context, req *providerpb.InitReq) (*providerpb.Enpty, error) {
	s.Impl.Init(Oauth2Option{
		ClientID:     req.ClientId,
		ClientSecret: req.ClientSecret,
		RedirectURL:  req.RedirectUrl,
	})
	return &providerpb.Enpty{}, nil
}

func (s *GRPCServer) Provider(ctx context.Context, req *providerpb.Enpty) (*providerpb.ProviderResp, error) {
	return &providerpb.ProviderResp{Name: string(s.Impl.Provider())}, nil
}

func (s *GRPCServer) NewAuthURL(ctx context.Context, req *providerpb.NewAuthURLReq) (*providerpb.NewAuthURLResp, error) {
	return &providerpb.NewAuthURLResp{Url: s.Impl.NewAuthURL(req.State)}, nil
}

func (s *GRPCServer) GetToken(ctx context.Context, req *providerpb.GetTokenReq) (*providerpb.Token, error) {
	token, err := s.Impl.GetToken(ctx, req.Code)
	if err != nil {
		return nil, err
	}
	return &providerpb.Token{
		AccessToken:  token.AccessToken,
		TokenType:    token.TokenType,
		RefreshToken: token.RefreshToken,
		Expiry:       token.Expiry.Unix(),
	}, nil
}

func (s *GRPCServer) GetUserInfo(ctx context.Context, req *providerpb.GetUserInfoReq) (*providerpb.GetUserInfoResp, error) {
	userInfo, err := s.Impl.GetUserInfo(ctx, &oauth2.Token{
		AccessToken:  req.Token.AccessToken,
		TokenType:    req.Token.TokenType,
		Expiry:       time.Unix(req.Token.Expiry, 0),
		RefreshToken: req.Token.RefreshToken,
	})
	if err != nil {
		return nil, err
	}
	resp := &providerpb.GetUserInfoResp{
		Username:       userInfo.Username,
		ProviderUserId: uint64(userInfo.ProviderUserID),
	}
	if userInfo.TokenRefreshed != nil {
		resp.TokenRefreshed = &providerpb.Token{
			AccessToken:  userInfo.TokenRefreshed.Token.AccessToken,
			TokenType:    userInfo.TokenRefreshed.Token.TokenType,
			RefreshToken: userInfo.TokenRefreshed.Token.RefreshToken,
			Expiry:       userInfo.TokenRefreshed.Token.Expiry.Unix(),
		}
	}
	return resp, nil
}

type GRPCClient struct{ client providerpb.Oauth2PluginClient }

var _ ProviderInterface = (*GRPCClient)(nil)

func (c *GRPCClient) Init(o Oauth2Option) {
	c.client.Init(context.Background(), &providerpb.InitReq{
		ClientId:     o.ClientID,
		ClientSecret: o.ClientSecret,
		RedirectUrl:  o.RedirectURL,
	})
}

func (c *GRPCClient) Provider() OAuth2Provider {
	resp, err := c.client.Provider(context.Background(), &providerpb.Enpty{})
	if err != nil {
		return ""
	}
	return OAuth2Provider(resp.Name)
}

func (c *GRPCClient) NewAuthURL(state string) string {
	resp, err := c.client.NewAuthURL(context.Background(), &providerpb.NewAuthURLReq{State: state})
	if err != nil {
		return ""
	}
	return resp.Url
}

func (c *GRPCClient) GetToken(ctx context.Context, code string) (*oauth2.Token, error) {
	resp, err := c.client.GetToken(ctx, &providerpb.GetTokenReq{Code: code})
	if err != nil {
		return nil, err
	}
	return &oauth2.Token{
		AccessToken:  resp.AccessToken,
		TokenType:    resp.TokenType,
		RefreshToken: resp.RefreshToken,
		Expiry:       time.Unix(resp.Expiry, 0),
	}, nil
}

func (c *GRPCClient) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*UserInfo, error) {
	resp, err := c.client.GetUserInfo(ctx, &providerpb.GetUserInfoReq{
		Token: &providerpb.Token{
			AccessToken:  tk.AccessToken,
			TokenType:    tk.TokenType,
			RefreshToken: tk.RefreshToken,
			Expiry:       tk.Expiry.Unix(),
		},
	})
	if err != nil {
		return nil, err
	}
	return &UserInfo{
		Username:       resp.Username,
		ProviderUserID: uint(resp.ProviderUserId),
	}, nil
}

var HandshakeConfig = plugin.HandshakeConfig{
	ProtocolVersion:  1,
	MagicCookieKey:   "BASIC_PLUGIN",
	MagicCookieValue: "hello",
}

var pluginMap = map[string]plugin.Plugin{
	"Provider": &ProviderPlugin{},
}

func InitProviderPlugins(name string, arg ...string) error {
	client := plugin.NewClient(&plugin.ClientConfig{
		HandshakeConfig: HandshakeConfig,
		Plugins:         pluginMap,
		Cmd:             exec.Command(name, arg...),
		AllowedProtocols: []plugin.Protocol{
			plugin.ProtocolGRPC},
		Logger: hclog.New(&hclog.LoggerOptions{
			Name:   "plugin",
			Output: log.StandardLogger().Writer(),
			Level:  hclog.Debug,
		}),
	})
	sysnotify.RegisterSysNotifyTask(0, sysnotify.NewSysNotifyTask("plugin", sysnotify.NotifyTypeEXIT, func() error {
		client.Kill()
		return nil
	}))
	c, err := client.Client()
	if err != nil {
		return err
	}
	i, err := c.Dispense("Provider")
	if err != nil {
		return err
	}
	provider, ok := i.(ProviderInterface)
	if !ok {
		return fmt.Errorf("%s not implement ProviderInterface", name)
	}
	registerProvider(provider)
	return nil
}
