package plugins

import (
	"context"

	"github.com/synctv-org/synctv/internal/provider"
	providerpb "github.com/synctv-org/synctv/proto/provider"
)

type GRPCClient struct{ client providerpb.Oauth2PluginClient }

var _ provider.ProviderInterface = (*GRPCClient)(nil)

func (c *GRPCClient) Init(o provider.Oauth2Option) {
	opt := providerpb.InitReq{
		ClientId:     o.ClientID,
		ClientSecret: o.ClientSecret,
		RedirectUrl:  o.RedirectURL,
	}
	c.client.Init(context.Background(), &opt)
}

func (c *GRPCClient) Provider() provider.OAuth2Provider {
	resp, err := c.client.Provider(context.Background(), &providerpb.Enpty{})
	if err != nil {
		return ""
	}
	return provider.OAuth2Provider(resp.Name)
}

func (c *GRPCClient) NewAuthURL(ctx context.Context, state string) (string, error) {
	resp, err := c.client.NewAuthURL(ctx, &providerpb.NewAuthURLReq{State: state})
	if err != nil {
		return "", err
	}
	return resp.Url, nil
}

func (c *GRPCClient) GetUserInfo(ctx context.Context, code string) (*provider.UserInfo, error) {
	resp, err := c.client.GetUserInfo(ctx, &providerpb.GetUserInfoReq{
		Code: code,
	})
	if err != nil {
		return nil, err
	}
	return &provider.UserInfo{
		Username:       resp.Username,
		ProviderUserID: resp.ProviderUserId,
	}, nil
}
