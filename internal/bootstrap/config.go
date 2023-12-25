package bootstrap

import (
	"context"
	"errors"
	"os"
	"path/filepath"
	"strings"

	"github.com/caarlos0/env/v9"
	"github.com/joho/godotenv"
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
	if flags.SkipConfig && flags.SkipEnv {
		log.Fatal("skip config and skip env at the same time")
		return errors.New("skip config and skip env at the same time")
	}
	conf.Conf = conf.DefaultConfig()
	if !flags.SkipConfig {
		configFile, err := utils.OptFilePath(filepath.Join(flags.DataDir, "config.yaml"))
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
	if !flags.SkipEnv {
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
	s, err := getEnvFiles(flags.DataDir)
	if err != nil {
		return err
	}
	if flags.Dev {
		ss, err := getEnvFiles(".")
		if err != nil {
			return err
		}
		s = append(s, ss...)
	}
	if len(s) != 0 {
		err = godotenv.Overload(s...)
		if err != nil {
			return err
		}
	}
	return env.ParseWithOptions(conf, env.Options{
		Prefix: prefix,
	})
}

func getEnvFiles(root string) ([]string, error) {
	var files []string

	err := filepath.Walk(root, func(path string, info os.FileInfo, err error) error {
		if err != nil {
			return err
		}

		if !info.IsDir() && strings.HasPrefix(info.Name(), ".env") {
			files = append(files, path)
		}

		return nil
	})

	if err != nil {
		return nil, err
	}

	return files, nil
}
