package cmd

import (
	"fmt"
	"net"
	"net/http"
	"time"

	"github.com/gin-gonic/gin"
	log "github.com/sirupsen/logrus"
	"github.com/soheilhy/cmux"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/rtmp"
	sysnotify "github.com/synctv-org/synctv/internal/sysnotify"
	"github.com/synctv-org/synctv/server"
)

var ServerCmd = &cobra.Command{
	Use:   "server",
	Short: "Start synctv-server",
	Long:  `Start synctv-server`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		boot := bootstrap.New().Add(
			bootstrap.InitSysNotify,
			bootstrap.InitConfig,
			bootstrap.InitGinMode,
			bootstrap.InitLog,
			bootstrap.InitDatabase,
			bootstrap.InitProvider,
			bootstrap.InitOp,
			bootstrap.InitRtmp,
			bootstrap.InitVendorBackend,
			bootstrap.InitSetting,
		)
		if !flags.Server.DisableUpdateCheck {
			boot.Add(bootstrap.InitCheckUpdate)
		}
		return boot.Run(cmd.Context())
	},
	Run: Server,
}

func setupAddresses() (tcpHTTPAddr, tcpRTMPAddr *net.TCPAddr, err error) {
	tcpHTTPAddr, err = net.ResolveTCPAddr(
		"tcp",
		fmt.Sprintf("%s:%d", conf.Conf.Server.HTTP.Listen, conf.Conf.Server.HTTP.Port),
	)
	if err != nil {
		return nil, nil, err
	}

	// Set default RTMP settings if not configured
	if conf.Conf.Server.RTMP.Listen == "" {
		conf.Conf.Server.RTMP.Listen = conf.Conf.Server.HTTP.Listen
	}

	if conf.Conf.Server.RTMP.Port == 0 {
		conf.Conf.Server.RTMP.Port = conf.Conf.Server.HTTP.Port
	}

	tcpRTMPAddr, err = net.ResolveTCPAddr(
		"tcp",
		fmt.Sprintf("%s:%d", conf.Conf.Server.RTMP.Listen, conf.Conf.Server.RTMP.Port),
	)

	return tcpHTTPAddr, tcpRTMPAddr, err
}

func startHTTPServer(e *gin.Engine, listener net.Listener) {
	switch {
	case conf.Conf.Server.HTTP.CertPath != "" && conf.Conf.Server.HTTP.KeyPath != "":
		go func() {
			srv := http.Server{Handler: e.Handler(), ReadHeaderTimeout: 3 * time.Second}

			err := srv.ServeTLS(
				listener,
				conf.Conf.Server.HTTP.CertPath,
				conf.Conf.Server.HTTP.KeyPath,
			)
			if err != nil {
				log.Panicf("http server error: %v", err)
			}
		}()
	case conf.Conf.Server.HTTP.CertPath == "" && conf.Conf.Server.HTTP.KeyPath == "":
		go func() {
			srv := http.Server{Handler: e.Handler(), ReadHeaderTimeout: 3 * time.Second}

			err := srv.Serve(listener)
			if err != nil {
				log.Panicf("http server error: %v", err)
			}
		}()
	default:
		log.Panic("cert and key must be both set")
	}
}

func Server(_ *cobra.Command, _ []string) {
	tcpHTTPAddr, tcpRTMPAddr, err := setupAddresses()
	if err != nil {
		log.Panic(err)
	}

	httpListener, err := net.ListenTCP("tcp", tcpHTTPAddr)
	if err != nil {
		log.Panic(err)
	}

	useMux := conf.Conf.Server.RTMP.Port == conf.Conf.Server.HTTP.Port &&
		conf.Conf.Server.RTMP.Listen == conf.Conf.Server.HTTP.Listen

	e := server.NewAndInit()

	if conf.Conf.Server.RTMP.Enable {
		if useMux {
			muxer := cmux.New(httpListener)

			// Setup HTTP
			var httpListener net.Listener
			if conf.Conf.Server.HTTP.CertPath != "" {
				httpListener = muxer.Match(cmux.HTTP2(), cmux.TLS())
			} else {
				httpListener = muxer.Match(cmux.HTTP1Fast())
			}

			startHTTPServer(e, httpListener)

			// Setup RTMP
			rtmpListener := muxer.Match(cmux.Any())
			go func() {
				err := rtmp.Server().Serve(rtmpListener)
				if err != nil {
					log.Panicf("rtmp server error: %v", err)
				}
			}()
			go func() {
				err := muxer.Serve()
				if err != nil {
					log.Panicf("mux server error: %v", err)
				}
			}()
		} else {
			// Separate listeners for HTTP and RTMP
			startHTTPServer(e, httpListener)

			rtmpListener, err := net.ListenTCP("tcp", tcpRTMPAddr)
			if err != nil {
				log.Fatal(err)
			}

			go func() {
				err := rtmp.Server().Serve(rtmpListener)
				if err != nil {
					log.Panicf("rtmp server error: %v", err)
				}
			}()
		}
	} else {
		startHTTPServer(e, httpListener)
	}

	// Log startup information
	if conf.Conf.Server.RTMP.Enable {
		log.Infof("rtmp run on tcp://%s:%d", tcpRTMPAddr.IP, tcpRTMPAddr.Port)
	}

	if conf.Conf.Server.HTTP.CertPath != "" && conf.Conf.Server.HTTP.KeyPath != "" {
		log.Infof("website run on https://%s:%d", tcpHTTPAddr.IP, tcpHTTPAddr.Port)
	} else {
		log.Infof("website run on http://%s:%d", tcpHTTPAddr.IP, tcpHTTPAddr.Port)
	}

	sysnotify.WaitCbk()
}

func init() {
	RootCmd.AddCommand(ServerCmd)
	ServerCmd.PersistentFlags().
		BoolVar(&flags.Server.DisableUpdateCheck, "disable-update-check", false, "disable update check")
	ServerCmd.PersistentFlags().
		BoolVar(&flags.Server.DisableWeb, "disable-web", false, "disable web")
	ServerCmd.PersistentFlags().
		BoolVar(&flags.Server.DisableLogColor, "disable-log-color", false, "disable log color")
	ServerCmd.PersistentFlags().
		StringVar(&flags.Server.WebPath, "web-path", "", "if not set, use embed web")
	ServerCmd.PersistentFlags().
		BoolVar(&flags.Server.SkipConfig, "skip-config", false, "skip config")
	ServerCmd.PersistentFlags().
		BoolVar(&flags.Server.SkipEnvConfig, "skip-env-config", false, "skip env config")
}
