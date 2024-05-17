package bootstrap

import (
	"context"
	"errors"
	"path/filepath"

	"github.com/caarlos0/env/v9"
	log "github.com/sirupsen/logrus"

	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/conf"
	"github.com/synctv-org/synctv/utils"
)

func InitDefaultConfig(ctx context.Context) error {
	conf.Conf = conf.DefaultConfig()
	return nil
}

func InitConfig(ctx context.Context) (err error) {
	if flags.Server.SkipConfig && flags.Server.SkipEnvConfig {
		log.Fatal("skip config and skip env at the same time")
		return errors.New("skip config and skip env at the same time")
	}
	conf.Conf = conf.DefaultConfig()
	if !flags.Server.SkipConfig {
		configFile, err := utils.OptFilePath(filepath.Join(flags.Global.DataDir, "config.yaml"))
		if err != nil {
			log.Fatalf("config file path error: %v", err)
		}
		err = confFromConfig(configFile, conf.Conf)
		if err != nil {
			log.Fatalf("load config from file error: %v", err)
		}
		log.Infof("load config success from file: %s", configFile)
		if err = restoreConfig(configFile, conf.Conf); err != nil {
			log.Warnf("restore config error: %v", err)
		} else {
			log.Info("restore config success")
		}
	}
	if !flags.Server.SkipEnvConfig {
		prefix := "SYNCTV_"
		if flags.EnvNoPrefix {
			prefix = ""
			log.Info("load config from env without prefix")
		} else {
			log.Infof("load config from env with prefix: %s", prefix)
		}
		err := confFromEnv(prefix, conf.Conf)
		if err != nil {
			log.Fatalf("load config from env error: %v", err)
		}
		log.Info("load config success from env")
	}
	return nil
}

func confFromConfig(filePath string, conf *conf.Config) error {
	if filePath == "" {
		return errors.New("config file path is empty")
	}
	if !utils.Exists(filePath) {
		log.Infof("config file not exists, create new config file: %s", filePath)
		err := conf.Save(filePath)
		if err != nil {
			return err
		}
	} else {
		err := utils.ReadYaml(filePath, conf)
		if err != nil {
			return err
		}
	}
	return nil
}

func restoreConfig(filePath string, conf *conf.Config) error {
	if filePath == "" {
		return errors.New("config file path is empty")
	}
	return conf.Save(filePath)
}

func confFromEnv(prefix string, conf *conf.Config) error {
	return env.ParseWithOptions(conf, env.Options{
		Prefix: prefix,
	})
}
