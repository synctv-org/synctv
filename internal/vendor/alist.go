package vendor

import (
	"context"
	"errors"

	"google.golang.org/grpc"

	"github.com/synctv-org/vendors/api/alist"
	alistService "github.com/synctv-org/vendors/service/alist"
)

type AlistInterface = alist.AlistHTTPServer

func LoadAlistClient(name string) AlistInterface {
	if cli, ok := clients.Load().alist[name]; ok {
		return cli
	}
	return alistLocalClient
}

var (
	alistLocalClient AlistInterface
)

func init() {
	alistLocalClient = alistService.NewAlistService(nil)
}

func AlistLocalClient() AlistInterface {
	return alistLocalClient
}

func NewAlistGrpcClient(conn *grpc.ClientConn) (AlistInterface, error) {
	if conn == nil {
		return nil, errors.New("grpc client conn is nil")
	}
	conn.GetState()
	return newGrpcAlist(alist.NewAlistClient(conn)), nil
}

var _ AlistInterface = (*grpcAlist)(nil)

type grpcAlist struct {
	client alist.AlistClient
}

func newGrpcAlist(client alist.AlistClient) AlistInterface {
	return &grpcAlist{
		client: client,
	}
}

func (a *grpcAlist) FsGet(ctx context.Context, req *alist.FsGetReq) (*alist.FsGetResp, error) {
	return a.client.FsGet(ctx, req)
}

func (a *grpcAlist) FsList(ctx context.Context, req *alist.FsListReq) (*alist.FsListResp, error) {
	return a.client.FsList(ctx, req)
}

func (a *grpcAlist) FsOther(ctx context.Context, req *alist.FsOtherReq) (*alist.FsOtherResp, error) {
	return a.client.FsOther(ctx, req)
}

func (a *grpcAlist) Login(ctx context.Context, req *alist.LoginReq) (*alist.LoginResp, error) {
	return a.client.Login(ctx, req)
}

func (a *grpcAlist) Me(ctx context.Context, req *alist.MeReq) (*alist.MeResp, error) {
	return a.client.Me(ctx, req)
}
