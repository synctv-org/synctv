package bootstrap

import (
	"context"
	"fmt"
	"io"
	"log"
	"os"
	"runtime"
	"time"

	"github.com/natefinch/lumberjack"
	"github.com/sirupsen/logrus"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/utils"
	"github.com/zijiren233/go-colorable"
)

func setLog(l *logrus.Logger) {
	if flags.Global.Dev {
		l.SetLevel(logrus.DebugLevel)
		l.SetReportCaller(true)
	} else {
		l.SetLevel(logrus.InfoLevel)
		l.SetReportCaller(false)
	}
}

var logCallerIgnoreFuncs = map[string]struct{}{
	"github.com/synctv-org/synctv/server/middlewares.logColor": {},
}

func InitLog(_ context.Context) (err error) {
	setLog(logrus.StandardLogger())
	forceColor := utils.ForceColor()
	if conf.Conf.Log.Enable {
		l := &lumberjack.Logger{
			Filename:   conf.Conf.Log.FilePath,
			MaxSize:    conf.Conf.Log.MaxSize,
			MaxBackups: conf.Conf.Log.MaxBackups,
			MaxAge:     conf.Conf.Log.MaxAge,
			Compress:   conf.Conf.Log.Compress,
		}
		if err := l.Rotate(); err != nil {
			logrus.Fatalf("log: rotate log file error: %v", err)
		}
		var w io.Writer
		if forceColor {
			w = colorable.NewNonColorableWriter(l)
		} else {
			w = l
		}
		if flags.Global.Dev || flags.Global.LogStd {
			logrus.SetOutput(io.MultiWriter(os.Stdout, w))
			logrus.Infof("log: enable log to stdout and file: %s", conf.Conf.Log.FilePath)
		} else {
			logrus.SetOutput(w)
			logrus.Infof("log: disable log to stdout, only log to file: %s", conf.Conf.Log.FilePath)
		}
	}
	switch conf.Conf.Log.LogFormat {
	case "json":
		logrus.SetFormatter(&logrus.JSONFormatter{
			TimestampFormat: time.DateTime,
			CallerPrettyfier: func(f *runtime.Frame) (function, file string) {
				if _, ok := logCallerIgnoreFuncs[f.Function]; ok {
					return "", ""
				}
				return f.Function, fmt.Sprintf("%s:%d", f.File, f.Line)
			},
		})
	default:
		if conf.Conf.Log.LogFormat != "text" {
			logrus.Warnf("unknown log format: %s, use default: text", conf.Conf.Log.LogFormat)
		}
		logrus.SetFormatter(&logrus.TextFormatter{
			ForceColors:      forceColor,
			DisableColors:    !forceColor,
			ForceQuote:       flags.Global.Dev,
			DisableQuote:     !flags.Global.Dev,
			DisableSorting:   false,
			FullTimestamp:    true,
			TimestampFormat:  time.DateTime,
			QuoteEmptyFields: true,
			CallerPrettyfier: func(f *runtime.Frame) (function, file string) {
				if _, ok := logCallerIgnoreFuncs[f.Function]; ok {
					return "", ""
				}
				return f.Function, fmt.Sprintf("%s:%d", f.File, f.Line)
			},
		})
	}
	log.SetOutput(logrus.StandardLogger().Writer())
	return nil
}

func InitStdLog(_ context.Context) error {
	logrus.StandardLogger().SetOutput(os.Stdout)
	log.SetOutput(os.Stdout)
	setLog(logrus.StandardLogger())
	return nil
}
