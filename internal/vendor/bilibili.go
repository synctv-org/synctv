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
	"github.com/synctv-org/vendors/api/bilibili"
	bilibiliService "github.com/synctv-org/vendors/service/bilibili"
	clientv3 "go.etcd.io/etcd/client/v3"
)

type BilibiliInterface = bilibili.BilibiliHTTPServer

func BilibiliClient(name string) BilibiliInterface {
	if name != "" {
		if cli, ok := bilibiliClients[name]; ok {
			return cli
		}
	}
	return bilibiliDefaultClient
}

func BilibiliClients() map[string]BilibiliInterface {
	return bilibiliClients
}

var (
	bilibiliClients       map[string]BilibiliInterface
	bilibiliDefaultClient BilibiliInterface
)

func InitBilibiliVendors(conf map[string]conf.BilibiliConfig) error {
	if bilibiliClients == nil {
		bilibiliClients = make(map[string]BilibiliInterface, len(conf))
	}
	for k, vb := range conf {
		cli, err := InitBilibili(&vb)
		if err != nil {
			return err
		}
		if k == "" {
			bilibiliDefaultClient = cli
		} else {
			bilibiliClients[k] = cli
		}
	}
	if bilibiliDefaultClient == nil {
		bilibiliDefaultClient = bilibiliService.NewBilibiliService(nil)
	}
	return nil
}

func InitBilibili(conf *conf.BilibiliConfig) (BilibiliInterface, error) {
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
			log.Infof("bilibili client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return nil, errors.New("bilibili server name is empty")
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
			log.Infof("bilibili client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return nil, errors.New("bilibili server name is empty")
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
			log.Infof("bilibili client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			return nil, errors.New("bilibili client init failed, endpoint is empty")
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
		return newGrpcBilibili(bilibili.NewBilibiliClient(con)), nil
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
			log.Infof("bilibili client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return nil, errors.New("bilibili server name is empty")
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
			log.Infof("bilibili client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return nil, errors.New("bilibili server name is empty")
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
			log.Infof("bilibili client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			return nil, errors.New("bilibili client init failed, endpoint is empty")
		}
		con, err := http.NewClient(
			context.Background(),
			opts...,
		)
		if err != nil {
			return nil, err
		}
		return newHTTPBilibili(bilibili.NewBilibiliHTTPClient(con)), nil
	default:
		return nil, errors.New("unknow bilibili scheme")
	}
}

var _ BilibiliInterface = (*grpcBilibili)(nil)

type grpcBilibili struct {
	client bilibili.BilibiliClient
}

func newGrpcBilibili(client bilibili.BilibiliClient) *grpcBilibili {
	return &grpcBilibili{
		client: client,
	}
}

func (g *grpcBilibili) NewQRCode(ctx context.Context, in *bilibili.Empty) (*bilibili.NewQRCodeResp, error) {
	return g.client.NewQRCode(ctx, in)
}

func (g *grpcBilibili) LoginWithQRCode(ctx context.Context, in *bilibili.LoginWithQRCodeReq) (*bilibili.LoginWithQRCodeResp, error) {
	return g.client.LoginWithQRCode(ctx, in)
}

func (g *grpcBilibili) NewCaptcha(ctx context.Context, in *bilibili.Empty) (*bilibili.NewCaptchaResp, error) {
	return g.client.NewCaptcha(ctx, in)
}

func (g *grpcBilibili) NewSMS(ctx context.Context, in *bilibili.NewSMSReq) (*bilibili.NewSMSResp, error) {
	return g.client.NewSMS(ctx, in)
}

func (g *grpcBilibili) LoginWithSMS(ctx context.Context, in *bilibili.LoginWithSMSReq) (*bilibili.LoginWithSMSResp, error) {
	return g.client.LoginWithSMS(ctx, in)
}

func (g *grpcBilibili) ParseVideoPage(ctx context.Context, in *bilibili.ParseVideoPageReq) (*bilibili.VideoPageInfo, error) {
	return g.client.ParseVideoPage(ctx, in)
}

func (g *grpcBilibili) GetVideoURL(ctx context.Context, in *bilibili.GetVideoURLReq) (*bilibili.VideoURL, error) {
	return g.client.GetVideoURL(ctx, in)
}

func (g *grpcBilibili) GetDashVideoURL(ctx context.Context, in *bilibili.GetDashVideoURLReq) (*bilibili.GetDashVideoURLResp, error) {
	return g.client.GetDashVideoURL(ctx, in)
}

func (g *grpcBilibili) GetSubtitles(ctx context.Context, in *bilibili.GetSubtitlesReq) (*bilibili.GetSubtitlesResp, error) {
	return g.client.GetSubtitles(ctx, in)
}

func (g *grpcBilibili) ParsePGCPage(ctx context.Context, in *bilibili.ParsePGCPageReq) (*bilibili.VideoPageInfo, error) {
	return g.client.ParsePGCPage(ctx, in)
}

func (g *grpcBilibili) GetPGCURL(ctx context.Context, in *bilibili.GetPGCURLReq) (*bilibili.VideoURL, error) {
	return g.client.GetPGCURL(ctx, in)
}

func (g *grpcBilibili) GetDashPGCURL(ctx context.Context, in *bilibili.GetDashPGCURLReq) (*bilibili.GetDashPGCURLResp, error) {
	return g.client.GetDashPGCURL(ctx, in)
}

func (g *grpcBilibili) UserInfo(ctx context.Context, in *bilibili.UserInfoReq) (*bilibili.UserInfoResp, error) {
	return g.client.UserInfo(ctx, in)
}

func (g *grpcBilibili) Match(ctx context.Context, in *bilibili.MatchReq) (*bilibili.MatchResp, error) {
	return g.client.Match(ctx, in)
}

var _ BilibiliInterface = (*httpBilibili)(nil)

type httpBilibili struct {
	client bilibili.BilibiliHTTPClient
}

func newHTTPBilibili(client bilibili.BilibiliHTTPClient) *httpBilibili {
	return &httpBilibili{
		client: client,
	}
}

func (h *httpBilibili) NewQRCode(ctx context.Context, in *bilibili.Empty) (*bilibili.NewQRCodeResp, error) {
	return h.client.NewQRCode(ctx, in)
}

func (h *httpBilibili) LoginWithQRCode(ctx context.Context, in *bilibili.LoginWithQRCodeReq) (*bilibili.LoginWithQRCodeResp, error) {
	return h.client.LoginWithQRCode(ctx, in)
}

func (h *httpBilibili) NewCaptcha(ctx context.Context, in *bilibili.Empty) (*bilibili.NewCaptchaResp, error) {
	return h.client.NewCaptcha(ctx, in)
}

func (h *httpBilibili) NewSMS(ctx context.Context, in *bilibili.NewSMSReq) (*bilibili.NewSMSResp, error) {
	return h.client.NewSMS(ctx, in)
}

func (h *httpBilibili) LoginWithSMS(ctx context.Context, in *bilibili.LoginWithSMSReq) (*bilibili.LoginWithSMSResp, error) {
	return h.client.LoginWithSMS(ctx, in)
}

func (h *httpBilibili) ParseVideoPage(ctx context.Context, in *bilibili.ParseVideoPageReq) (*bilibili.VideoPageInfo, error) {
	return h.client.ParseVideoPage(ctx, in)
}

func (h *httpBilibili) GetVideoURL(ctx context.Context, in *bilibili.GetVideoURLReq) (*bilibili.VideoURL, error) {
	return h.client.GetVideoURL(ctx, in)
}

func (h *httpBilibili) GetDashVideoURL(ctx context.Context, in *bilibili.GetDashVideoURLReq) (*bilibili.GetDashVideoURLResp, error) {
	return h.client.GetDashVideoURL(ctx, in)
}

func (h *httpBilibili) GetSubtitles(ctx context.Context, in *bilibili.GetSubtitlesReq) (*bilibili.GetSubtitlesResp, error) {
	return h.client.GetSubtitles(ctx, in)
}

func (h *httpBilibili) ParsePGCPage(ctx context.Context, in *bilibili.ParsePGCPageReq) (*bilibili.VideoPageInfo, error) {
	return h.client.ParsePGCPage(ctx, in)
}

func (h *httpBilibili) GetPGCURL(ctx context.Context, in *bilibili.GetPGCURLReq) (*bilibili.VideoURL, error) {
	return h.client.GetPGCURL(ctx, in)
}

func (h *httpBilibili) GetDashPGCURL(ctx context.Context, in *bilibili.GetDashPGCURLReq) (*bilibili.GetDashPGCURLResp, error) {
	return h.client.GetDashPGCURL(ctx, in)
}

func (h *httpBilibili) UserInfo(ctx context.Context, in *bilibili.UserInfoReq) (*bilibili.UserInfoResp, error) {
	return h.client.UserInfo(ctx, in)
}

func (h *httpBilibili) Match(ctx context.Context, in *bilibili.MatchReq) (*bilibili.MatchResp, error) {
	return h.client.Match(ctx, in)
}
