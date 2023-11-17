package plugins

import (
	"context"
	"time"

	"github.com/synctv-org/synctv/internal/provider"
	providerpb "github.com/synctv-org/synctv/proto/provider"
	"golang.org/x/oauth2"
)

type GRPCClient struct{ client providerpb.Oauth2PluginClient }

var _ provider.ProviderInterface = (*GRPCClient)(nil)

func (c *GRPCClient) Init(o provider.Oauth2Option) {
	c.client.Init(context.Background(), &providerpb.InitReq{
		ClientId:     o.ClientID,
		ClientSecret: o.ClientSecret,
		RedirectUrl:  o.RedirectURL,
	})
}

func (c *GRPCClient) Provider() provider.OAuth2Provider {
	resp, err := c.client.Provider(context.Background(), &providerpb.Enpty{})
	if err != nil {
		return ""
	}
	return provider.OAuth2Provider(resp.Name)
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

func (c *GRPCClient) RefreshToken(ctx context.Context, tk string) (*oauth2.Token, error) {
	resp, err := c.client.RefreshToken(ctx, &providerpb.RefreshTokenReq{
		RefreshToken: tk,
	})
	if err != nil {
		return nil, err
	}
	return &oauth2.Token{
		AccessToken:  resp.Token.AccessToken,
		TokenType:    resp.Token.TokenType,
		RefreshToken: resp.Token.RefreshToken,
		Expiry:       time.Unix(resp.Token.Expiry, 0),
	}, nil
}

func (c *GRPCClient) GetUserInfo(ctx context.Context, tk *oauth2.Token) (*provider.UserInfo, error) {
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
	return &provider.UserInfo{
		Username:       resp.Username,
		ProviderUserID: resp.ProviderUserId,
	}, nil
}
