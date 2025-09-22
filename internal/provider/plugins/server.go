package plugins

import (
	"context"

	"github.com/synctv-org/synctv/internal/provider"
	providerpb "github.com/synctv-org/synctv/proto/provider"
)

type GRPCServer struct {
	providerpb.UnimplementedOauth2PluginServer
	Impl provider.Interface
}

func (s *GRPCServer) Init(_ context.Context, req *providerpb.InitReq) (*providerpb.Enpty, error) {
	opt := provider.Oauth2Option{
		ClientID:     req.GetClientId(),
		ClientSecret: req.GetClientSecret(),
		RedirectURL:  req.GetRedirectUrl(),
	}
	s.Impl.Init(opt)

	return &providerpb.Enpty{}, nil
}

func (s *GRPCServer) Provider(
	_ context.Context,
	_ *providerpb.Enpty,
) (*providerpb.ProviderResp, error) {
	return &providerpb.ProviderResp{Name: s.Impl.Provider()}, nil
}

func (s *GRPCServer) NewAuthURL(
	ctx context.Context,
	req *providerpb.NewAuthURLReq,
) (*providerpb.NewAuthURLResp, error) {
	s2, err := s.Impl.NewAuthURL(ctx, req.GetState())
	if err != nil {
		return nil, err
	}

	return &providerpb.NewAuthURLResp{Url: s2}, nil
}

func (s *GRPCServer) GetUserInfo(
	ctx context.Context,
	req *providerpb.GetUserInfoReq,
) (*providerpb.GetUserInfoResp, error) {
	userInfo, err := s.Impl.GetUserInfo(ctx, req.GetCode())
	if err != nil {
		return nil, err
	}

	resp := &providerpb.GetUserInfoResp{
		Username:       userInfo.Username,
		ProviderUserId: userInfo.ProviderUserID,
	}

	return resp, nil
}
