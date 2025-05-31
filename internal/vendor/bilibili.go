package vendor

import (
	"context"
	"errors"

	"github.com/synctv-org/vendors/api/bilibili"
	bilibiliService "github.com/synctv-org/vendors/service/bilibili"
	"google.golang.org/grpc"
)

type BilibiliInterface = bilibili.BilibiliHTTPServer

func LoadBilibiliClient(name string) BilibiliInterface {
	if cli, ok := LoadClients().bilibili[name]; ok {
		return cli
	}
	return bilibiliLocalClient
}

var bilibiliLocalClient BilibiliInterface

func init() {
	bilibiliLocalClient = bilibiliService.NewBilibiliService(nil)
}

func BilibiliLocalClient() BilibiliInterface {
	return bilibiliLocalClient
}

func NewBilibiliGrpcClient(conn *grpc.ClientConn) (BilibiliInterface, error) {
	if conn == nil {
		return nil, errors.New("grpc client conn is nil")
	}
	return newGrpcBilibili(bilibili.NewBilibiliClient(conn)), nil
}

var _ BilibiliInterface = (*grpcBilibili)(nil)

type grpcBilibili struct {
	client bilibili.BilibiliClient
}

func newGrpcBilibili(client bilibili.BilibiliClient) BilibiliInterface {
	return &grpcBilibili{
		client: client,
	}
}

func (g *grpcBilibili) GetLiveDanmuInfo(
	ctx context.Context,
	in *bilibili.GetLiveDanmuInfoReq,
) (*bilibili.GetLiveDanmuInfoResp, error) {
	return g.client.GetLiveDanmuInfo(ctx, in)
}

func (g *grpcBilibili) NewQRCode(
	ctx context.Context,
	in *bilibili.Empty,
) (*bilibili.NewQRCodeResp, error) {
	return g.client.NewQRCode(ctx, in)
}

func (g *grpcBilibili) LoginWithQRCode(
	ctx context.Context,
	in *bilibili.LoginWithQRCodeReq,
) (*bilibili.LoginWithQRCodeResp, error) {
	return g.client.LoginWithQRCode(ctx, in)
}

func (g *grpcBilibili) NewCaptcha(
	ctx context.Context,
	in *bilibili.Empty,
) (*bilibili.NewCaptchaResp, error) {
	return g.client.NewCaptcha(ctx, in)
}

func (g *grpcBilibili) NewSMS(
	ctx context.Context,
	in *bilibili.NewSMSReq,
) (*bilibili.NewSMSResp, error) {
	return g.client.NewSMS(ctx, in)
}

func (g *grpcBilibili) LoginWithSMS(
	ctx context.Context,
	in *bilibili.LoginWithSMSReq,
) (*bilibili.LoginWithSMSResp, error) {
	return g.client.LoginWithSMS(ctx, in)
}

func (g *grpcBilibili) ParseVideoPage(
	ctx context.Context,
	in *bilibili.ParseVideoPageReq,
) (*bilibili.VideoPageInfo, error) {
	return g.client.ParseVideoPage(ctx, in)
}

func (g *grpcBilibili) GetVideoURL(
	ctx context.Context,
	in *bilibili.GetVideoURLReq,
) (*bilibili.VideoURL, error) {
	return g.client.GetVideoURL(ctx, in)
}

func (g *grpcBilibili) GetDashVideoURL(
	ctx context.Context,
	in *bilibili.GetDashVideoURLReq,
) (*bilibili.GetDashVideoURLResp, error) {
	return g.client.GetDashVideoURL(ctx, in)
}

func (g *grpcBilibili) GetSubtitles(
	ctx context.Context,
	in *bilibili.GetSubtitlesReq,
) (*bilibili.GetSubtitlesResp, error) {
	return g.client.GetSubtitles(ctx, in)
}

func (g *grpcBilibili) ParsePGCPage(
	ctx context.Context,
	in *bilibili.ParsePGCPageReq,
) (*bilibili.VideoPageInfo, error) {
	return g.client.ParsePGCPage(ctx, in)
}

func (g *grpcBilibili) GetPGCURL(
	ctx context.Context,
	in *bilibili.GetPGCURLReq,
) (*bilibili.VideoURL, error) {
	return g.client.GetPGCURL(ctx, in)
}

func (g *grpcBilibili) GetDashPGCURL(
	ctx context.Context,
	in *bilibili.GetDashPGCURLReq,
) (*bilibili.GetDashPGCURLResp, error) {
	return g.client.GetDashPGCURL(ctx, in)
}

func (g *grpcBilibili) UserInfo(
	ctx context.Context,
	in *bilibili.UserInfoReq,
) (*bilibili.UserInfoResp, error) {
	return g.client.UserInfo(ctx, in)
}

func (g *grpcBilibili) Match(
	ctx context.Context,
	in *bilibili.MatchReq,
) (*bilibili.MatchResp, error) {
	return g.client.Match(ctx, in)
}

func (g *grpcBilibili) GetLiveStreams(
	ctx context.Context,
	in *bilibili.GetLiveStreamsReq,
) (*bilibili.GetLiveStreamsResp, error) {
	return g.client.GetLiveStreams(ctx, in)
}

func (g *grpcBilibili) ParseLivePage(
	ctx context.Context,
	req *bilibili.ParseLivePageReq,
) (*bilibili.VideoPageInfo, error) {
	return g.client.ParseLivePage(ctx, req)
}
