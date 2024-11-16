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

func (s *GRPCServer) Init(ctx context.Context, req *providerpb.InitReq) (*providerpb.Enpty, error) {
	opt := provider.Oauth2Option{
		ClientID:     req.ClientId,
		ClientSecret: req.ClientSecret,
		RedirectURL:  req.RedirectUrl,
	}
	s.Impl.Init(opt)
	return &providerpb.Enpty{}, nil
}

func (s *GRPCServer) Provider(ctx context.Context, req *providerpb.Enpty) (*providerpb.ProviderResp, error) {
	return &providerpb.ProviderResp{Name: s.Impl.Provider()}, nil
}

func (s *GRPCServer) NewAuthURL(ctx context.Context, req *providerpb.NewAuthURLReq) (*providerpb.NewAuthURLResp, error) {
	s2, err := s.Impl.NewAuthURL(ctx, req.State)
	if err != nil {
		return nil, err
	}
	return &providerpb.NewAuthURLResp{Url: s2}, nil
}

func (s *GRPCServer) GetUserInfo(ctx context.Context, req *providerpb.GetUserInfoReq) (*providerpb.GetUserInfoResp, error) {
	userInfo, err := s.Impl.GetUserInfo(ctx, req.Code)
	if err != nil {
		return nil, err
	}
	resp := &providerpb.GetUserInfoResp{
		Username:       userInfo.Username,
		ProviderUserId: userInfo.ProviderUserID,
	}
	return resp, nil
}
