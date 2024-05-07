package vendor

import (
	"context"
	"errors"

	"google.golang.org/grpc"

	"github.com/synctv-org/vendors/api/emby"
	embyService "github.com/synctv-org/vendors/service/emby"
)

type EmbyInterface = emby.EmbyHTTPServer

func LoadEmbyClient(name string) EmbyInterface {
	if cli, ok := LoadClients().emby[name]; ok && cli != nil {
		return cli
	}
	return embyLocalClient
}

var (
	embyLocalClient EmbyInterface
)

func init() {
	embyLocalClient = embyService.NewEmbyService(nil)
}

func EmbyLocalClient() EmbyInterface {
	return embyLocalClient
}

func NewEmbyGrpcClient(conn *grpc.ClientConn) (EmbyInterface, error) {
	if conn == nil {
		return nil, errors.New("grpc client conn is nil")
	}
	conn.GetState()
	return newGrpcEmby(emby.NewEmbyClient(conn)), nil
}

var _ EmbyInterface = (*grpcEmby)(nil)

type grpcEmby struct {
	client emby.EmbyClient
}

func newGrpcEmby(client emby.EmbyClient) EmbyInterface {
	return &grpcEmby{
		client: client,
	}
}

func (e *grpcEmby) FsList(ctx context.Context, req *emby.FsListReq) (*emby.FsListResp, error) {
	return e.client.FsList(ctx, req)
}

func (e *grpcEmby) GetItem(ctx context.Context, req *emby.GetItemReq) (*emby.Item, error) {
	return e.client.GetItem(ctx, req)
}

func (e *grpcEmby) GetItems(ctx context.Context, req *emby.GetItemsReq) (*emby.GetItemsResp, error) {
	return e.client.GetItems(ctx, req)
}

func (e *grpcEmby) GetSystemInfo(ctx context.Context, req *emby.SystemInfoReq) (*emby.SystemInfoResp, error) {
	return e.client.GetSystemInfo(ctx, req)
}

func (e *grpcEmby) Login(ctx context.Context, req *emby.LoginReq) (*emby.LoginResp, error) {
	return e.client.Login(ctx, req)
}

func (e *grpcEmby) Logout(ctx context.Context, req *emby.LogoutReq) (*emby.Empty, error) {
	return e.client.Logout(ctx, req)
}

func (e *grpcEmby) Me(ctx context.Context, req *emby.MeReq) (*emby.MeResp, error) {
	return e.client.Me(ctx, req)
}

func (e *grpcEmby) PlaybackInfo(ctx context.Context, req *emby.PlaybackInfoReq) (*emby.PlaybackInfoResp, error) {
	return e.client.PlaybackInfo(ctx, req)
}

func (e *grpcEmby) DeleteActiveEncodeings(ctx context.Context, req *emby.DeleteActiveEncodeingsReq) (*emby.Empty, error) {
	return e.client.DeleteActiveEncodeings(ctx, req)
}
