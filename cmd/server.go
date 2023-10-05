package cmd

import (
	"fmt"
	"net"
	"net/http"

	"github.com/quic-go/quic-go/http3"
	log "github.com/sirupsen/logrus"
	"github.com/soheilhy/cmux"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/server"
)

var ServerCmd = &cobra.Command{
	Use:               "server",
	Short:             "Start synctv-server",
	Long:              `Start synctv-server`,
	PersistentPreRunE: Init,
	PreRunE:           func(cmd *cobra.Command, args []string) error { return InitGinMode() },
	Run:               Server,
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
	if conf.Conf.Rtmp.Enable {
		if useMux {
			muxer := cmux.New(serverListener)
			e, s := server.NewAndInit()
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
			go s.Serve(tcp)
			go muxer.Serve()
		} else {
			e, s := server.NewAndInit()
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
			go s.Serve(rtmpListener)
		}
	} else {
		e, _ := server.NewAndInit()
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
	bootstrap.SysNotify.WaitCbk()
}

func init() {
	RootCmd.AddCommand(ServerCmd)
}
