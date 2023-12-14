package vendor

import (
	"context"
	"crypto/tls"
	"crypto/x509"
	"errors"
	"fmt"
	"os"
	"sync/atomic"
	"time"

	"github.com/go-kratos/aegis/circuitbreaker"
	"github.com/go-kratos/aegis/circuitbreaker/sre"
	consul "github.com/go-kratos/kratos/contrib/registry/consul/v2"
	"github.com/go-kratos/kratos/contrib/registry/etcd/v2"
	klog "github.com/go-kratos/kratos/v2/log"
	"github.com/go-kratos/kratos/v2/middleware"
	"github.com/go-kratos/kratos/v2/middleware/auth/jwt"
	kcircuitbreaker "github.com/go-kratos/kratos/v2/middleware/circuitbreaker"
	"github.com/go-kratos/kratos/v2/selector"
	"github.com/go-kratos/kratos/v2/selector/wrr"
	ggrpc "github.com/go-kratos/kratos/v2/transport/grpc"
	"github.com/go-kratos/kratos/v2/transport/http"
	jwtv4 "github.com/golang-jwt/jwt/v4"
	"github.com/hashicorp/consul/api"
	log "github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/internal/model"
	clientv3 "go.etcd.io/etcd/client/v3"
	"google.golang.org/grpc"
)

func init() {
	klog.SetLogger(klog.NewStdLogger(log.StandardLogger().Writer()))
	selector.SetGlobalSelector(wrr.NewBuilder())
}

var backends atomic.Pointer[Backends]

type BackendConnInfo struct {
	Conn *grpc.ClientConn
	Info *model.VendorBackend
}

type Backends struct {
	conns    map[string]*BackendConnInfo
	bilibili map[string]BilibiliInterface
	alist    map[string]AlistInterface
	emby     map[string]EmbyInterface
}

func (b *Backends) Conns() map[string]*BackendConnInfo {
	return b.conns
}

func (b *Backends) BilibiliClients() map[string]BilibiliInterface {
	return b.bilibili
}

func (b *Backends) AlistClients() map[string]AlistInterface {
	return b.alist
}

func (b *Backends) EmbyClients() map[string]EmbyInterface {
	return b.emby
}

func NewBackends(ctx context.Context, conf []*model.VendorBackend) (*Backends, error) {
	newConns := make(map[string]*BackendConnInfo, len(conf))
	backends := &Backends{
		conns:    newConns,
		bilibili: make(map[string]BilibiliInterface),
		alist:    make(map[string]AlistInterface),
		emby:     make(map[string]EmbyInterface),
	}
	for _, vb := range conf {
		cc, err := NewGrpcClientConn(ctx, &vb.Backend)
		if err != nil {
			return nil, err
		}
		if _, ok := newConns[vb.Backend.Endpoint]; ok {
			return nil, fmt.Errorf("duplicate endpoint: %s", vb.Backend.Endpoint)
		}
		newConns[vb.Backend.Endpoint] = &BackendConnInfo{
			Conn: cc,
			Info: vb,
		}
		if vb.UsedBy.Bilibili {
			if _, ok := backends.bilibili[vb.UsedBy.BilibiliBackendName]; ok {
				return nil, fmt.Errorf("duplicate bilibili backend name: %s", vb.UsedBy.BilibiliBackendName)
			}
			cli, err := NewBilibiliGrpcClient(cc)
			if err != nil {
				return nil, err
			}
			backends.bilibili[vb.UsedBy.BilibiliBackendName] = cli
		}
		if vb.UsedBy.Alist {
			if _, ok := backends.alist[vb.UsedBy.AlistBackendName]; ok {
				return nil, fmt.Errorf("duplicate alist backend name: %s", vb.UsedBy.AlistBackendName)
			}
			cli, err := NewAlistGrpcClient(cc)
			if err != nil {
				return nil, err
			}
			backends.alist[vb.UsedBy.AlistBackendName] = cli
		}
		if vb.UsedBy.Emby {
			if _, ok := backends.emby[vb.UsedBy.EmbyBackendName]; ok {
				return nil, fmt.Errorf("duplicate emby backend name: %s", vb.UsedBy.EmbyBackendName)
			}
			cli, err := NewEmbyGrpcClient(cc)
			if err != nil {
				return nil, err
			}
			backends.emby[vb.UsedBy.EmbyBackendName] = cli
		}
	}

	return backends, nil
}

func LoadBackends() *Backends {
	return backends.Load()
}

func StoreBackends(b *Backends) {
	old := backends.Swap(b)
	if old == nil {
		return
	}
	for k, conn := range old.conns {
		conn.Conn.Close()
		delete(old.conns, k)
	}
}

func NewGrpcClientConn(ctx context.Context, conf *model.Backend) (*grpc.ClientConn, error) {
	if conf.Endpoint == "" {
		return nil, errors.New("new grpc client failed, endpoint is empty")
	}
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

	opts := []ggrpc.ClientOption{
		ggrpc.WithMiddleware(middlewares...),
		// ggrpc.WithOptions(grpc.WithBlock()),
	}

	if conf.TimeOut != "" {
		timeout, err := time.ParseDuration(conf.TimeOut)
		if err != nil {
			return nil, err
		}
		opts = append(opts, ggrpc.WithTimeout(timeout))
	}

	if conf.Consul.ServerName != "" {
		c := api.DefaultConfig()
		c.Address = conf.Endpoint
		c.Token = conf.Consul.Token
		c.TokenFile = conf.Consul.TokenFile
		c.PathPrefix = conf.Consul.PathPrefix
		c.Namespace = conf.Consul.Namespace
		c.Partition = conf.Consul.Partition
		client, err := api.NewClient(c)
		if err != nil {
			return nil, err
		}
		endpoint := fmt.Sprintf("discovery:///%s", conf.Consul.ServerName)
		dis := consul.New(client)
		opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
		log.Infof("new grpc client with consul: %s", conf.Endpoint)
	} else if conf.Etcd.ServerName != "" {
		endpoint := fmt.Sprintf("discovery:///%s", conf.Etcd.ServerName)
		cli, err := clientv3.New(clientv3.Config{
			Endpoints: []string{conf.Endpoint},
			Username:  conf.Etcd.Username,
			Password:  conf.Etcd.Password,
		})
		if err != nil {
			return nil, err
		}
		dis := etcd.New(cli)
		opts = append(opts, ggrpc.WithEndpoint(endpoint), ggrpc.WithDiscovery(dis))
		log.Infof("new grpc client with etcd: %v", conf.Endpoint)
	} else {
		opts = append(opts, ggrpc.WithEndpoint(conf.Endpoint))
		log.Infof("new grpc client with endpoint: %s", conf.Endpoint)
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
				return nil, err
			}
			rootCAs.AppendCertsFromPEM(b)
		}
		opts = append(opts, ggrpc.WithTLSConfig(&tls.Config{
			RootCAs: rootCAs,
		}))

		con, err = ggrpc.Dial(
			ctx,
			opts...,
		)
	} else {
		con, err = ggrpc.DialInsecure(
			ctx,
			opts...,
		)
	}
	if err != nil {
		return nil, err
	}
	return con, nil
}

func NewHttpClientConn(ctx context.Context, conf *model.Backend) (*http.Client, error) {
	if conf.Endpoint == "" {
		return nil, errors.New("new http client failed, endpoint is empty")
	}
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

	opts := []http.ClientOption{
		http.WithMiddleware(middlewares...),
	}

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
				return nil, err
			}
			rootCAs.AppendCertsFromPEM(b)
		}
		opts = append(opts, http.WithTLSConfig(&tls.Config{
			RootCAs: rootCAs,
		}))
	}

	if conf.Consul.ServerName != "" {
		c := api.DefaultConfig()
		c.Address = conf.Endpoint
		c.Token = conf.Consul.Token
		c.TokenFile = conf.Consul.TokenFile
		c.PathPrefix = conf.Consul.PathPrefix
		c.Namespace = conf.Consul.Namespace
		c.Partition = conf.Consul.Partition
		client, err := api.NewClient(c)
		if err != nil {
			return nil, err
		}
		endpoint := fmt.Sprintf("discovery:///%s", conf.Consul.ServerName)
		dis := consul.New(client)
		opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
		log.Infof("new http client with consul: %s", conf.Endpoint)
	} else if conf.Etcd.ServerName != "" {
		endpoint := fmt.Sprintf("discovery:///%s", conf.Etcd.ServerName)
		cli, err := clientv3.New(clientv3.Config{
			Endpoints: []string{conf.Endpoint},
			Username:  conf.Etcd.Username,
			Password:  conf.Etcd.Password,
		})
		if err != nil {
			return nil, err
		}
		dis := etcd.New(cli)
		opts = append(opts, http.WithEndpoint(endpoint), http.WithDiscovery(dis))
		log.Infof("new http client with etcd: %v", conf.Endpoint)
	} else {
		opts = append(opts, http.WithEndpoint(conf.Endpoint))
		log.Infof("new http client with endpoint: %s", conf.Endpoint)
	}

	con, err := http.NewClient(
		ctx,
		opts...,
	)
	if err != nil {
		return nil, err
	}
	return con, nil
}
