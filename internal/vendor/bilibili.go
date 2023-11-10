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

func BilibiliClient() Bilibili {
	return bilibiliClient
}

var (
	bilibiliClient        Bilibili
	bilibiliDefaultClient Bilibili
)

type Bilibili interface {
	NewQRCode(ctx context.Context, in *bilibili.Empty) (*bilibili.NewQRCodeResp, error)
	LoginWithQRCode(ctx context.Context, in *bilibili.LoginWithQRCodeReq) (*bilibili.LoginWithQRCodeResp, error)
	NewCaptcha(ctx context.Context, in *bilibili.Empty) (*bilibili.NewCaptchaResp, error)
	NewSMS(ctx context.Context, in *bilibili.NewSMSReq) (*bilibili.NewSMSResp, error)
	LoginWithSMS(ctx context.Context, in *bilibili.LoginWithSMSReq) (*bilibili.LoginWithSMSResp, error)
	ParseVideoPage(ctx context.Context, in *bilibili.ParseVideoPageReq) (*bilibili.VideoPageInfo, error)
	GetVideoURL(ctx context.Context, in *bilibili.GetVideoURLReq) (*bilibili.VideoURL, error)
	GetDashVideoURL(ctx context.Context, in *bilibili.GetDashVideoURLReq) (*bilibili.GetDashVideoURLResp, error)
	GetSubtitles(ctx context.Context, in *bilibili.GetSubtitlesReq) (*bilibili.GetSubtitlesResp, error)
	ParsePGCPage(ctx context.Context, in *bilibili.ParsePGCPageReq) (*bilibili.VideoPageInfo, error)
	GetPGCURL(ctx context.Context, in *bilibili.GetPGCURLReq) (*bilibili.VideoURL, error)
	GetDashPGCURL(ctx context.Context, in *bilibili.GetDashPGCURLReq) (*bilibili.GetDashPGCURLResp, error)
	UserInfo(ctx context.Context, in *bilibili.UserInfoReq) (*bilibili.UserInfoResp, error)
	Match(ctx context.Context, in *bilibili.MatchReq) (*bilibili.MatchResp, error)
}

var (
	b = sre.NewBreaker(
		sre.WithRequest(25),
		sre.WithWindow(time.Second*15),
	)
	circuitBreaker = kcircuitbreaker.Client(kcircuitbreaker.WithCircuitBreaker(func() circuitbreaker.CircuitBreaker {
		return b
	}))
)

func InitBilibili(conf *conf.Bilibili) error {
	key := []byte(conf.JwtSecret)
	bilibiliDefaultClient = bilibiliService.NewBilibiliService(nil)
	switch conf.Scheme {
	case "grpc":
		opts := []ggrpc.ClientOption{}

		if conf.JwtSecret != "" {
			opts = append(opts, ggrpc.WithMiddleware(
				jwt.Client(func(token *jwtv4.Token) (interface{}, error) {
					return key, nil
				}, jwt.WithSigningMethod(jwtv4.SigningMethodHS256)),
				circuitBreaker,
			))
		}

		if conf.TimeOut != "" {
			timeout, err := time.ParseDuration(conf.TimeOut)
			if err != nil {
				return err
			}
			opts = append(opts, ggrpc.WithTimeout(timeout))
		}

		if conf.Endpoint != "" {
			opts = append(opts, ggrpc.WithEndpoint(conf.Endpoint))
			log.Infof("bilibili client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return errors.New("bilibili server name is empty")
			}
			c := api.DefaultConfig()
			c.Address = conf.Consul.Endpoint
			client, err := api.NewClient(c)
			if err != nil {
				return err
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			dis := consul.New(client)
			opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
			log.Infof("bilibili client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return errors.New("bilibili server name is empty")
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			cli, err := clientv3.New(clientv3.Config{
				Endpoints: conf.Etcd.Endpoints,
				Username:  conf.Etcd.Username,
				Password:  conf.Etcd.Password,
			})
			if err != nil {
				return err
			}
			dis := etcd.New(cli)
			opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
			log.Infof("bilibili client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			bilibiliClient = bilibiliDefaultClient
			return nil
		}
		var (
			con *grpc.ClientConn
			err error
		)
		if conf.Tls {
			var rootCAs *x509.CertPool
			rootCAs, err = x509.SystemCertPool()
			if err != nil {
				fmt.Println("Failed to load system root CA:", err)
				return err
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
			return err
		}
		bilibiliClient = newGrpcBilibili(bilibili.NewBilibiliClient(con))
	case "http":
		opts := []http.ClientOption{}
		if conf.Tls {
			rootCAs, err := x509.SystemCertPool()
			if err != nil {
				fmt.Println("Failed to load system root CA:", err)
				return err
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

		if conf.JwtSecret != "" {
			opts = append(opts, http.WithMiddleware(
				jwt.Client(func(token *jwtv4.Token) (interface{}, error) {
					return key, nil
				}, jwt.WithSigningMethod(jwtv4.SigningMethodHS256)),
				circuitBreaker,
			))
		}

		if conf.TimeOut != "" {
			timeout, err := time.ParseDuration(conf.TimeOut)
			if err != nil {
				return err
			}
			opts = append(opts, http.WithTimeout(timeout))
		}

		if conf.Endpoint != "" {
			opts = append(opts, http.WithEndpoint(conf.Endpoint))
			log.Infof("bilibili client init success with endpoint: %s", conf.Endpoint)
		} else if conf.Consul.Endpoint != "" {
			if conf.ServerName == "" {
				return errors.New("bilibili server name is empty")
			}
			c := api.DefaultConfig()
			c.Address = conf.Consul.Endpoint
			client, err := api.NewClient(c)
			if err != nil {
				return err
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			dis := consul.New(client)
			opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
			log.Infof("bilibili client init success with consul: %s", conf.Consul.Endpoint)
		} else if len(conf.Etcd.Endpoints) > 0 {
			if conf.ServerName == "" {
				return errors.New("bilibili server name is empty")
			}
			endpoint := fmt.Sprintf("discovery:///%s", conf.ServerName)
			cli, err := clientv3.New(clientv3.Config{
				Endpoints: conf.Etcd.Endpoints,
				Username:  conf.Etcd.Username,
				Password:  conf.Etcd.Password,
			})
			if err != nil {
				return err
			}
			dis := etcd.New(cli)
			opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
			log.Infof("bilibili client init success with etcd: %v", conf.Etcd.Endpoints)
		} else {
			bilibiliClient = bilibiliDefaultClient
			return nil
		}
		con, err := http.NewClient(
			context.Background(),
			opts...,
		)
		if err != nil {
			return err
		}
		bilibiliClient = newHTTPBilibili(bilibili.NewBilibiliHTTPClient(con))
	default:
		return errors.New("unknow bilibili scheme")
	}

	return nil
}

var _ Bilibili = (*grpcBilibili)(nil)

type grpcBilibili struct {
	client bilibili.BilibiliClient
}

func newGrpcBilibili(client bilibili.BilibiliClient) *grpcBilibili {
	return &grpcBilibili{
		client: client,
	}
}

func (g *grpcBilibili) NewQRCode(ctx context.Context, in *bilibili.Empty) (*bilibili.NewQRCodeResp, error) {
	resp, err := g.client.NewQRCode(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewQRCode(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) LoginWithQRCode(ctx context.Context, in *bilibili.LoginWithQRCodeReq) (*bilibili.LoginWithQRCodeResp, error) {
	resp, err := g.client.LoginWithQRCode(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.LoginWithQRCode(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) NewCaptcha(ctx context.Context, in *bilibili.Empty) (*bilibili.NewCaptchaResp, error) {
	resp, err := g.client.NewCaptcha(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewCaptcha(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) NewSMS(ctx context.Context, in *bilibili.NewSMSReq) (*bilibili.NewSMSResp, error) {
	resp, err := g.client.NewSMS(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewSMS(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) LoginWithSMS(ctx context.Context, in *bilibili.LoginWithSMSReq) (*bilibili.LoginWithSMSResp, error) {
	resp, err := g.client.LoginWithSMS(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.LoginWithSMS(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) ParseVideoPage(ctx context.Context, in *bilibili.ParseVideoPageReq) (*bilibili.VideoPageInfo, error) {
	resp, err := g.client.ParseVideoPage(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.ParseVideoPage(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) GetVideoURL(ctx context.Context, in *bilibili.GetVideoURLReq) (*bilibili.VideoURL, error) {
	resp, err := g.client.GetVideoURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetVideoURL(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) GetDashVideoURL(ctx context.Context, in *bilibili.GetDashVideoURLReq) (*bilibili.GetDashVideoURLResp, error) {
	resp, err := g.client.GetDashVideoURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetDashVideoURL(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) GetSubtitles(ctx context.Context, in *bilibili.GetSubtitlesReq) (*bilibili.GetSubtitlesResp, error) {
	resp, err := g.client.GetSubtitles(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetSubtitles(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) ParsePGCPage(ctx context.Context, in *bilibili.ParsePGCPageReq) (*bilibili.VideoPageInfo, error) {
	resp, err := g.client.ParsePGCPage(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.ParsePGCPage(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) GetPGCURL(ctx context.Context, in *bilibili.GetPGCURLReq) (*bilibili.VideoURL, error) {
	resp, err := g.client.GetPGCURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetPGCURL(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) GetDashPGCURL(ctx context.Context, in *bilibili.GetDashPGCURLReq) (*bilibili.GetDashPGCURLResp, error) {
	resp, err := g.client.GetDashPGCURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetDashPGCURL(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) UserInfo(ctx context.Context, in *bilibili.UserInfoReq) (*bilibili.UserInfoResp, error) {
	resp, err := g.client.UserInfo(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.UserInfo(ctx, in)
	}
	return resp, err
}

func (g *grpcBilibili) Match(ctx context.Context, in *bilibili.MatchReq) (*bilibili.MatchResp, error) {
	resp, err := g.client.Match(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.Match(ctx, in)
	}
	return resp, err
}

var _ Bilibili = (*httpBilibili)(nil)

type httpBilibili struct {
	client bilibili.BilibiliHTTPClient
}

func newHTTPBilibili(client bilibili.BilibiliHTTPClient) *httpBilibili {
	return &httpBilibili{
		client: client,
	}
}

func (h *httpBilibili) NewQRCode(ctx context.Context, in *bilibili.Empty) (*bilibili.NewQRCodeResp, error) {
	resp, err := h.client.NewQRCode(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewQRCode(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) LoginWithQRCode(ctx context.Context, in *bilibili.LoginWithQRCodeReq) (*bilibili.LoginWithQRCodeResp, error) {
	resp, err := h.client.LoginWithQRCode(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.LoginWithQRCode(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) NewCaptcha(ctx context.Context, in *bilibili.Empty) (*bilibili.NewCaptchaResp, error) {
	resp, err := h.client.NewCaptcha(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewCaptcha(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) NewSMS(ctx context.Context, in *bilibili.NewSMSReq) (*bilibili.NewSMSResp, error) {
	resp, err := h.client.NewSMS(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.NewSMS(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) LoginWithSMS(ctx context.Context, in *bilibili.LoginWithSMSReq) (*bilibili.LoginWithSMSResp, error) {
	resp, err := h.client.LoginWithSMS(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.LoginWithSMS(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) ParseVideoPage(ctx context.Context, in *bilibili.ParseVideoPageReq) (*bilibili.VideoPageInfo, error) {
	resp, err := h.client.ParseVideoPage(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.ParseVideoPage(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) GetVideoURL(ctx context.Context, in *bilibili.GetVideoURLReq) (*bilibili.VideoURL, error) {
	resp, err := h.client.GetVideoURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetVideoURL(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) GetDashVideoURL(ctx context.Context, in *bilibili.GetDashVideoURLReq) (*bilibili.GetDashVideoURLResp, error) {
	resp, err := h.client.GetDashVideoURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetDashVideoURL(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) GetSubtitles(ctx context.Context, in *bilibili.GetSubtitlesReq) (*bilibili.GetSubtitlesResp, error) {
	resp, err := h.client.GetSubtitles(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetSubtitles(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) ParsePGCPage(ctx context.Context, in *bilibili.ParsePGCPageReq) (*bilibili.VideoPageInfo, error) {
	resp, err := h.client.ParsePGCPage(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.ParsePGCPage(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) GetPGCURL(ctx context.Context, in *bilibili.GetPGCURLReq) (*bilibili.VideoURL, error) {
	resp, err := h.client.GetPGCURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetPGCURL(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) GetDashPGCURL(ctx context.Context, in *bilibili.GetDashPGCURLReq) (*bilibili.GetDashPGCURLResp, error) {
	resp, err := h.client.GetDashPGCURL(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.GetDashPGCURL(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) UserInfo(ctx context.Context, in *bilibili.UserInfoReq) (*bilibili.UserInfoResp, error) {
	resp, err := h.client.UserInfo(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.UserInfo(ctx, in)
	}
	return resp, err
}

func (h *httpBilibili) Match(ctx context.Context, in *bilibili.MatchReq) (*bilibili.MatchResp, error) {
	resp, err := h.client.Match(ctx, in)
	if err != nil && errors.Is(err, kcircuitbreaker.ErrNotAllowed) {
		return bilibiliDefaultClient.Match(ctx, in)
	}
	return resp, err
}
