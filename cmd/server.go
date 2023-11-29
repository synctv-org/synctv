package cmd

import (
	"fmt"
	"net"
	"net/http"

	"github.com/quic-go/quic-go/http3"
	log "github.com/sirupsen/logrus"
	"github.com/soheilhy/cmux"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/internal/rtmp"
	sysnotify "github.com/synctv-org/synctv/internal/sysNotify"
	"github.com/synctv-org/synctv/server"
	"github.com/synctv-org/synctv/utils"
)

var ServerCmd = &cobra.Command{
	Use:   "server",
	Short: "Start synctv-server",
	Long:  `Start synctv-server`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		boot := bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitSysNotify,
			bootstrap.InitConfig,
			bootstrap.InitLog,
			bootstrap.InitGinMode,
			bootstrap.InitDatabase,
			bootstrap.InitProvider,
			bootstrap.InitOp,
			bootstrap.InitRtmp,
			bootstrap.InitVendor,
			bootstrap.InitSetting,
		)
		if !flags.DisableUpdateCheck {
			boot.Add(bootstrap.InitCheckUpdate)
		}
		return boot.Run()
	},
	Run: Server,
}

func Server(cmd *cobra.Command, args []string) {
	tcpServerHttpAddr, err := net.ResolveTCPAddr("tcp", fmt.Sprintf("%s:%d", conf.Conf.Server.Http.Listen, conf.Conf.Server.Http.Port))
	if err != nil {
		log.Panic(err)
	}
	udpServerHttpAddr, err := net.ResolveUDPAddr("udp", fmt.Sprintf("%s:%d", conf.Conf.Server.Http.Listen, conf.Conf.Server.Http.Port))
	if err != nil {
		log.Panic(err)
	}
	serverHttpListener, err := net.ListenTCP("tcp", tcpServerHttpAddr)
	if err != nil {
		log.Panic(err)
	}

	if conf.Conf.Server.Rtmp.Listen == "" {
		conf.Conf.Server.Rtmp.Listen = conf.Conf.Server.Http.Listen
	}
	if conf.Conf.Server.Rtmp.Port == 0 {
		conf.Conf.Server.Rtmp.Port = conf.Conf.Server.Http.Port
	}
	var useMux bool
	if conf.Conf.Server.Rtmp.Port == conf.Conf.Server.Http.Port && conf.Conf.Server.Rtmp.Listen == conf.Conf.Server.Http.Listen {
		useMux = true
		conf.Conf.Server.Rtmp.Port = conf.Conf.Server.Http.Port
		conf.Conf.Server.Rtmp.Listen = conf.Conf.Server.Http.Listen
	}

	serverRtmpAddr, err := net.ResolveTCPAddr("tcp", fmt.Sprintf("%s:%d", conf.Conf.Server.Rtmp.Listen, conf.Conf.Server.Rtmp.Port))
	if err != nil {
		log.Fatal(err)
	}
	if conf.Conf.Server.Rtmp.Enable {
		if useMux {
			muxer := cmux.New(serverHttpListener)
			e := server.NewAndInit()
			switch {
			case conf.Conf.Server.Http.CertPath != "" && conf.Conf.Server.Http.KeyPath != "":
				conf.Conf.Server.Http.CertPath, err = utils.OptFilePath(conf.Conf.Server.Http.CertPath)
				if err != nil {
					log.Fatalf("cert path error: %s", err)
				}
				conf.Conf.Server.Http.KeyPath, err = utils.OptFilePath(conf.Conf.Server.Http.KeyPath)
				if err != nil {
					log.Fatalf("key path error: %s", err)
				}
				httpl := muxer.Match(cmux.HTTP2(), cmux.TLS())
				go http.ServeTLS(httpl, e.Handler(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath)
				if conf.Conf.Server.Http.Quic {
					go http3.ListenAndServeQUIC(udpServerHttpAddr.String(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath, e.Handler())
				}
			case conf.Conf.Server.Http.CertPath == "" && conf.Conf.Server.Http.KeyPath == "":
				httpl := muxer.Match(cmux.HTTP1Fast())
				go e.RunListener(httpl)
			default:
				log.Panic("cert and key must be both set")
			}
			tcp := muxer.Match(cmux.Any())
			go rtmp.RtmpServer().Serve(tcp)
			go muxer.Serve()
		} else {
			e := server.NewAndInit()
			switch {
			case conf.Conf.Server.Http.CertPath != "" && conf.Conf.Server.Http.KeyPath != "":
				conf.Conf.Server.Http.CertPath, err = utils.OptFilePath(conf.Conf.Server.Http.CertPath)
				if err != nil {
					log.Fatalf("cert path error: %s", err)
				}
				conf.Conf.Server.Http.KeyPath, err = utils.OptFilePath(conf.Conf.Server.Http.KeyPath)
				if err != nil {
					log.Fatalf("key path error: %s", err)
				}
				go http.ServeTLS(serverHttpListener, e.Handler(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath)
				if conf.Conf.Server.Http.Quic {
					go http3.ListenAndServeQUIC(udpServerHttpAddr.String(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath, e.Handler())
				}
			case conf.Conf.Server.Http.CertPath == "" && conf.Conf.Server.Http.KeyPath == "":
				go e.RunListener(serverHttpListener)
			default:
				log.Panic("cert and key must be both set")
			}
			rtmpListener, err := net.ListenTCP("tcp", serverRtmpAddr)
			if err != nil {
				log.Fatal(err)
			}
			go rtmp.RtmpServer().Serve(rtmpListener)
		}
	} else {
		e := server.NewAndInit()
		switch {
		case conf.Conf.Server.Http.CertPath != "" && conf.Conf.Server.Http.KeyPath != "":
			go http.ServeTLS(serverHttpListener, e.Handler(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath)
			if conf.Conf.Server.Http.Quic {
				go http3.ListenAndServeQUIC(udpServerHttpAddr.String(), conf.Conf.Server.Http.CertPath, conf.Conf.Server.Http.KeyPath, e.Handler())
			}
		case conf.Conf.Server.Http.CertPath == "" && conf.Conf.Server.Http.KeyPath == "":
			go e.RunListener(serverHttpListener)
		default:
			log.Panic("cert and key must be both set")
		}
	}
	if conf.Conf.Server.Rtmp.Enable {
		log.Infof("rtmp run on tcp://%s:%d", serverRtmpAddr.IP, serverRtmpAddr.Port)
	}
	if conf.Conf.Server.Http.CertPath != "" && conf.Conf.Server.Http.KeyPath != "" {
		if conf.Conf.Server.Http.Quic {
			log.Infof("quic run on udp://%s:%d", udpServerHttpAddr.IP, udpServerHttpAddr.Port)
		}
		log.Infof("website run on https://%s:%d", tcpServerHttpAddr.IP, tcpServerHttpAddr.Port)
	} else {
		log.Infof("website run on http://%s:%d", tcpServerHttpAddr.IP, tcpServerHttpAddr.Port)
	}
	sysnotify.WaitCbk()
}

func init() {
	RootCmd.AddCommand(ServerCmd)
	ServerCmd.PersistentFlags().BoolVar(&flags.DisableUpdateCheck, "disable-update-check", false, "disable update check")
}
