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
		)
		if !flags.DisableUpdateCheck {
			boot.Add(bootstrap.InitCheckUpdate)
		}
		return boot.Run()
	},
	Run: Server,
}

func Server(cmd *cobra.Command, args []string) {
	tcpServerAddr, err := net.ResolveTCPAddr("tcp", fmt.Sprintf("%s:%d", conf.Conf.Server.Listen, conf.Conf.Server.Port))
	if err != nil {
		log.Panic(err)
	}
	udpServerAddr, err := net.ResolveUDPAddr("udp", fmt.Sprintf("%s:%d", conf.Conf.Server.Listen, conf.Conf.Server.Port))
	if err != nil {
		log.Panic(err)
	}
	serverListener, err := net.ListenTCP("tcp", tcpServerAddr)
	if err != nil {
		log.Panic(err)
	}
	var useMux bool
	if conf.Conf.Rtmp.Port == 0 || conf.Conf.Rtmp.Port == conf.Conf.Server.Port {
		useMux = true
		conf.Conf.Rtmp.Port = conf.Conf.Server.Port
	}
	tcpRtmpAddr, err := net.ResolveTCPAddr("tcp", fmt.Sprintf("%s:%d", conf.Conf.Server.Listen, conf.Conf.Rtmp.Port))
	if err != nil {
		log.Fatal(err)
	}
	utils.OptFilePath(&conf.Conf.Server.CertPath)
	utils.OptFilePath(&conf.Conf.Server.KeyPath)
	if conf.Conf.Rtmp.Enable {
		if useMux {
			muxer := cmux.New(serverListener)
			e := server.NewAndInit()
			switch {
			case conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "":
				httpl := muxer.Match(cmux.HTTP2(), cmux.TLS())
				go http.ServeTLS(httpl, e.Handler(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath)
				if conf.Conf.Server.Quic {
					go http3.ListenAndServeQUIC(udpServerAddr.String(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath, e.Handler())
				}
			case conf.Conf.Server.CertPath == "" && conf.Conf.Server.KeyPath == "":
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
			case conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "":
				go http.ServeTLS(serverListener, e.Handler(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath)
				if conf.Conf.Server.Quic {
					go http3.ListenAndServeQUIC(udpServerAddr.String(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath, e.Handler())
				}
			case conf.Conf.Server.CertPath == "" && conf.Conf.Server.KeyPath == "":
				go e.RunListener(serverListener)
			default:
				log.Panic("cert and key must be both set")
			}
			rtmpListener, err := net.ListenTCP("tcp", tcpRtmpAddr)
			if err != nil {
				log.Fatal(err)
			}
			go rtmp.RtmpServer().Serve(rtmpListener)
		}
	} else {
		e := server.NewAndInit()
		switch {
		case conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "":
			go http.ServeTLS(serverListener, e.Handler(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath)
			if conf.Conf.Server.Quic {
				go http3.ListenAndServeQUIC(udpServerAddr.String(), conf.Conf.Server.CertPath, conf.Conf.Server.KeyPath, e.Handler())
			}
		case conf.Conf.Server.CertPath == "" && conf.Conf.Server.KeyPath == "":
			go e.RunListener(serverListener)
		default:
			log.Panic("cert and key must be both set")
		}
	}
	if conf.Conf.Rtmp.Enable {
		log.Infof("rtmp run on tcp://%s:%d", tcpServerAddr.IP, tcpRtmpAddr.Port)
	}
	if conf.Conf.Server.CertPath != "" && conf.Conf.Server.KeyPath != "" {
		if conf.Conf.Server.Quic {
			log.Infof("quic run on udp://%s:%d", udpServerAddr.IP, udpServerAddr.Port)
		}
		log.Infof("website run on https://%s:%d", tcpServerAddr.IP, tcpServerAddr.Port)
	} else {
		log.Infof("website run on http://%s:%d", tcpServerAddr.IP, tcpServerAddr.Port)
	}
	sysnotify.WaitCbk()
}

func init() {
	RootCmd.AddCommand(ServerCmd)
	ServerCmd.PersistentFlags().BoolVar(&flags.DisableUpdateCheck, "disable-update-check", false, "disable update check")
}
