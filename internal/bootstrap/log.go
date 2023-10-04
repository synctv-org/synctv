package bootstrap

import (
	"io"
	"log"
	"os"

	"github.com/natefinch/lumberjack"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/zijiren233/go-colorable"
)

func setLog(l *logrus.Logger) {
	if flags.Dev {
		l.SetLevel(logrus.DebugLevel)
		l.SetReportCaller(true)
	} else {
		l.SetLevel(logrus.InfoLevel)
		l.SetReportCaller(false)
	}
}

func InitLog() {
	setLog(logrus.StandardLogger())
	logConfig := conf.Conf.Log
	if logConfig.Enable {
		var l = &lumberjack.Logger{
			Filename:   logConfig.FilePath,
			MaxSize:    logConfig.MaxSize,
			MaxBackups: logConfig.MaxBackups,
			MaxAge:     logConfig.MaxAge,
			Compress:   logConfig.Compress,
		}
		if err := l.Rotate(); err != nil {
			logrus.Fatalf("log: rotate log file error: %v", err)
		}
		var w io.Writer = colorable.NewNonColorableWriter(l)
		if flags.Dev || flags.LogStd {
			w = io.MultiWriter(os.Stdout, w)
		} else {
			logrus.Infof("log: disable log to stdout, only log to file: %s", logConfig.FilePath)
		}
		logrus.SetOutput(w)
	}
	switch conf.Conf.Log.LogFormat {
	case "json":
		logrus.SetFormatter(&logrus.JSONFormatter{})
	default:
		if conf.Conf.Log.LogFormat != "text" {
			logrus.Warnf("unknown log format: %s, use default: text", conf.Conf.Log.LogFormat)
		}
		if colorable.IsTerminal(os.Stdout.Fd()) {
			logrus.SetFormatter(&logrus.TextFormatter{
				ForceColors: true,
			})
		} else {
			logrus.SetFormatter(&logrus.TextFormatter{})
		}
	}
	log.SetOutput(logrus.StandardLogger().Out)
}
