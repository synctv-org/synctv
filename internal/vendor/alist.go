package vendor

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"time"

	"github.com/go-kratos/aegis/circuitbreaker"
	"github.com/go-kratos/aegis/circuitbreaker/sre"
	consul "github.com/go-kratos/kratos/contrib/registry/consul/v2"
	"github.com/go-kratos/kratos/contrib/registry/etcd/v2"
	"github.com/go-kratos/kratos/v2/middleware"
	"github.com/go-kratos/kratos/v2/middleware/auth/jwt"
	kcircuitbreaker "github.com/go-kratos/kratos/v2/middleware/circuitbreaker"
	"google.golang.org/grpc"

	ggrpc "github.com/go-kratos/kratos/v2/transport/grpc"
	"github.com/go-kratos/kratos/v2/transport/http"
	jwtv4 "github.com/golang-jwt/jwt/v4"
	"github.com/hashicorp/consul/api"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/vendors/api/alist"
	alistService "github.com/synctv-org/vendors/service/alist"
	clientv3 "go.etcd.io/etcd/client/v3"
)

type AlistInterface = alist.AlistHTTPServer

func AlistClient(name string) AlistInterface {
	if name != "" {
		if cli, ok := alistClients[name]; ok {
			return cli
		}
	}
	return alistDefaultClient
}

func AlistClients() map[string]AlistInterface {
	return alistClients
}

var (
	alistClients       map[string]AlistInterface
	alistDefaultClient AlistInterface
)

func InitAlistVendors(conf map[string]conf.AlistConfig) error {
	if alistClients == nil {
		alistClients = make(map[string]AlistInterface, len(conf))
	}
	for k, vb := range conf {
		cli, err := InitAlist(&vb)
		if err != nil {
			return err
		}
		if k == "" {
			alistDefaultClient = cli
		} else {
			alistClients[k] = cli
		}
	}
	if alistDefaultClient == nil {
		alistDefaultClient = alistService.NewAlistService(nil)
	}
	return nil
}

func InitAlist(conf *conf.AlistConfig) (AlistInterface, error) {
	middlewares := []middleware.Middleware{kcircuitbreaker.Client(kcircuitbreaker.WithCircuitBreaker(func() circuitbreaker.CircuitBreaker {
		return sre.NewBreaker(
			sre.WithRequest(25),
			sre.WithWindow(time.Second*15),
		)
	}))}

	if conf.JwtSecret != "" {
		key := []byte(conf.JwtSecret)
		middlewares = append(middlewares, jwt.Client(func(token *jwtv4.Token) (interface{}, error) {
			return key, nil
		}, jwt.WithSigningMethod(jwtv4.SigningMethodHS256)))
	}

	switch conf.Scheme {
	case "grpc":
		opts := []ggrpc.ClientOption{}

		opts = append(opts, ggrpc.WithMiddleware(middlewares...))

		if conf.TimeOut != "" {
			timeout, err := time.ParseDuration(conf.TimeOut)
			if err != nil {
				return nil, err
			}
			opts = append(opts, ggrpc.WithTimeout(timeout))
		}

		if conf.Endpoint != "" {
			opts = append(opts, ggrpc.WithEndpoint(conf.Endpoint))
			log.Infof("alist client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return nil, errors.New("alist server name is empty")
			}
			c := api.DefaultConfig()
			c.Address = conf.Consul.Endpoint
			client, err := api.NewClient(c)
			if err != nil {
				return nil, err
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			dis := consul.New(client)
			opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
			log.Infof("alist client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return nil, errors.New("alist server name is empty")
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			cli, err := clientv3.New(clientv3.Config{
				Endpoints: conf.Etcd.Endpoints,
				Username:  conf.Etcd.Username,
				Password:  conf.Etcd.Password,
			})
			if err != nil {
				return nil, err
			}
			dis := etcd.New(cli)
			opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
			log.Infof("alist client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			return nil, errors.New("alist client init failed, endpoint is empty")
		}
		var (
			con *grpc.ClientConn
			err error
		)
		if conf.Tls {
			var rootCAs *x509.CertPool
			rootCAs, err = x509.SystemCertPool()
			if err != nil {
				return nil, err
			}
			if conf.CustomCAFile != "" {
				b, err := os.ReadFile(conf.CustomCAFile)
				if err != nil {
					panic(err)
				}
				rootCAs.AppendCertsFromPEM(b)
			}
			opts = append(opts, ggrpc.WithTLSConfig(&tls.Config{
				RootCAs: rootCAs,
			}))

			con, err = ggrpc.Dial(
				context.Background(),
				opts...,
			)
		} else {
			con, err = ggrpc.DialInsecure(
				context.Background(),
				opts...,
			)
		}
		if err != nil {
			return nil, err
		}
		return newGrpcAlist(alist.NewAlistClient(con)), nil
	case "http":
		opts := []http.ClientOption{}

		opts = append(opts, http.WithMiddleware(middlewares...))

		if conf.TimeOut != "" {
			timeout, err := time.ParseDuration(conf.TimeOut)
			if err != nil {
				return nil, err
			}
			opts = append(opts, http.WithTimeout(timeout))
		}

		if conf.Tls {
			rootCAs, err := x509.SystemCertPool()
			if err != nil {
				return nil, err
			}
			if conf.CustomCAFile != "" {
				b, err := os.ReadFile(conf.CustomCAFile)
				if err != nil {
					panic(err)
				}
				rootCAs.AppendCertsFromPEM(b)
			}
			opts = append(opts, http.WithTLSConfig(&tls.Config{
				RootCAs: rootCAs,
			}))
		}

		if conf.Endpoint != "" {
			opts = append(opts, http.WithEndpoint(conf.Endpoint))
			log.Infof("alist client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return nil, errors.New("alist server name is empty")
			}
			c := api.DefaultConfig()
			c.Address = conf.Consul.Endpoint
			client, err := api.NewClient(c)
			if err != nil {
				return nil, err
			}
			c.Token = conf.Consul.Token
			c.TokenFile = conf.Consul.TokenFile
			c.PathPrefix = conf.Consul.PathPrefix
			c.Namespace = conf.Consul.Namespace
			c.Partition = conf.Consul.Partition
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			dis := consul.New(client)
			opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
			log.Infof("alist client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return nil, errors.New("alist server name is empty")
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			cli, err := clientv3.New(clientv3.Config{
				Endpoints: conf.Etcd.Endpoints,
				Username:  conf.Etcd.Username,
				Password:  conf.Etcd.Password,
			})
			if err != nil {
				return nil, err
			}
			dis := etcd.New(cli)
			opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
			log.Infof("alist client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			return nil, errors.New("alist client init failed, endpoint is empty")
		}
		con, err := http.NewClient(
			context.Background(),
			opts...,
		)
		if err != nil {
			return nil, err
		}
		return newHTTPAlist(alist.NewAlistHTTPClient(con)), nil
	default:
		return nil, errors.New("unknow alist scheme")
	}
}

var _ AlistInterface = (*grpcAlist)(nil)

type grpcAlist struct {
	client alist.AlistClient
}

func newGrpcAlist(client alist.AlistClient) *grpcAlist {
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

var _ AlistInterface = (*httpAlist)(nil)

type httpAlist struct {
	client alist.AlistHTTPClient
}

func newHTTPAlist(client alist.AlistHTTPClient) *httpAlist {
	return &httpAlist{
		client: client,
	}
}

func (a *httpAlist) FsGet(ctx context.Context, req *alist.FsGetReq) (*alist.FsGetResp, error) {
	return a.client.FsGet(ctx, req)
}

func (a *httpAlist) FsList(ctx context.Context, req *alist.FsListReq) (*alist.FsListResp, error) {
	return a.client.FsList(ctx, req)
}

func (a *httpAlist) FsOther(ctx context.Context, req *alist.FsOtherReq) (*alist.FsOtherResp, error) {
	return a.client.FsOther(ctx, req)
}

func (a *httpAlist) Login(ctx context.Context, req *alist.LoginReq) (*alist.LoginResp, error) {
	return a.client.Login(ctx, req)
}
