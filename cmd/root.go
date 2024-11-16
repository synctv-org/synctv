package cmd

import (
	"fmt"
	"os"
	"path/filepath"
	"strings"

	"github.com/caarlos0/env/v9"
	"github.com/go-kratos/kratos/v2/log"
	"github.com/joho/godotenv"
	"github.com/mitchellh/go-homedir"
	"github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/cmd/admin"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/cmd/root"
	"github.com/synctv-org/synctv/cmd/setting"
	"github.com/synctv-org/synctv/cmd/user"
	"github.com/synctv-org/synctv/internal/version"
	"github.com/synctv-org/synctv/utils"
)

var RootCmd = &cobra.Command{
	Use:   "synctv",
	Short: "synctv",
	Long:  `synctv https://github.com/synctv-org/synctv`,
	PersistentPreRun: func(cmd *cobra.Command, args []string) {
		prefix := flags.EnvPrefix
		if !flags.SkipEnvFlag {
			s, ok := os.LookupEnv("ENV_NO_PREFIX")
			if ok {
				if strings.ToLower(s) == "true" {
					flags.EnvNoPrefix = true
				}
			}
			if flags.EnvNoPrefix {
				prefix = ""
				log.Info("load flags from env without prefix")
			} else {
				log.Infof("load flags from env with prefix: %s", prefix)
			}
		}
		if !flags.SkipEnvFlag {
			dataDir, ok := os.LookupEnv(prefix + "DATA_DIR")
			if ok {
				flags.Global.DataDir = dataDir
			}
			dev, ok := os.LookupEnv(prefix + "DEV")
			if ok {
				if strings.ToLower(dev) == "true" {
					flags.Global.Dev = true
				}
			}
		}

		envFiles, err := utils.GetEnvFiles(flags.Global.DataDir)
		if err != nil {
			logrus.Warnf("get env files error: %v", err)
		}
		if flags.Global.Dev {
			moreEnvFiles, err := utils.GetEnvFiles(".")
			if err != nil {
				logrus.Warnf("get env files error: %v", err)
			}
			envFiles = append(envFiles, moreEnvFiles...)
		}
		if len(envFiles) != 0 {
			log.Infof("load env from: %v", envFiles)
			err = godotenv.Load(envFiles...)
			if err != nil {
				logrus.Fatalf("load env error: %v", err)
			}
		}

		if !flags.SkipEnvFlag {
			err := env.ParseWithOptions(&flags.Global, env.Options{Prefix: prefix})
			if err != nil {
				logrus.Fatalf("parse env error: %v", err)
			}
			err = env.ParseWithOptions(&flags.Server, env.Options{Prefix: prefix})
			if err != nil {
				logrus.Fatalf("parse env error: %v", err)
			}
		}
	},
}

func Execute() {
	if err := RootCmd.Execute(); err != nil {
		fmt.Fprintln(os.Stderr, err)
		os.Exit(1)
	}
}

func init() {
	RootCmd.PersistentFlags().BoolVar(&flags.Global.Dev, "dev", version.Version == "dev", "start with dev mode")
	RootCmd.PersistentFlags().BoolVar(&flags.Global.LogStd, "log-std", true, "log to std")
	RootCmd.PersistentFlags().BoolVar(&flags.EnvNoPrefix, "env-no-prefix", false, "env no SYNCTV_ prefix")
	RootCmd.PersistentFlags().BoolVar(&flags.SkipEnvFlag, "skip-env-flag", true, "skip env flag")
	RootCmd.PersistentFlags().StringVar(&flags.Global.GitHubBaseURL, "github-base-url", "https://api.github.com/", "github api base url")
	home, err := homedir.Dir()
	if err != nil {
		home = "~"
	}
	RootCmd.PersistentFlags().StringVar(&flags.Global.DataDir, "data-dir", filepath.Join(home, ".synctv"), "data dir")
	RootCmd.PersistentFlags().BoolVar(&flags.Global.ForceAutoMigrate, "force-auto-migrate", version.Version == "dev", "force auto migrate")
}

func init() {
	RootCmd.AddCommand(admin.AdminCmd)
	RootCmd.AddCommand(user.UserCmd)
	RootCmd.AddCommand(setting.SettingCmd)
	RootCmd.AddCommand(root.RootCmd)
}
