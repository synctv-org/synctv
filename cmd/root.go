package cmd

import (
	"fmt"
	"os"
	"path/filepath"

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
		s, err := utils.GetEnvFiles(flags.DataDir)
		if err != nil {
			logrus.Fatalf("get env files error: %v", err)
		}
		if flags.Dev {
			ss, err := utils.GetEnvFiles(".")
			if err != nil {
				logrus.Fatalf("get env files error: %v", err)
			}
			s = append(s, ss...)
		}
		if len(s) != 0 {
			log.Infof("Overload env from: %v", s)
			err = godotenv.Overload(s...)
			if err != nil {
				logrus.Fatalf("load env error: %v", err)
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
	RootCmd.PersistentFlags().BoolVar(&flags.Dev, "dev", version.Version == "dev", "start with dev mode")
	RootCmd.PersistentFlags().BoolVar(&flags.LogStd, "log-std", true, "log to std")
	RootCmd.PersistentFlags().BoolVar(&flags.EnvNoPrefix, "env-no-prefix", false, "env no SYNCTV_ prefix")
	RootCmd.PersistentFlags().BoolVar(&flags.SkipConfig, "skip-config", false, "skip config")
	RootCmd.PersistentFlags().BoolVar(&flags.SkipEnv, "skip-env", false, "skip env")
	RootCmd.PersistentFlags().StringVar(&flags.GitHubBaseURL, "github-base-url", "https://api.github.com/", "github api base url")
	home, err := homedir.Dir()
	if err != nil {
		home = "~"
	}
	RootCmd.PersistentFlags().StringVar(&flags.DataDir, "data-dir", filepath.Join(home, ".synctv"), "data dir")
	RootCmd.PersistentFlags().BoolVar(&flags.ForceAutoMigrate, "force-auto-migrate", version.Version == "dev", "force auto migrate")
}

func init() {
	RootCmd.AddCommand(admin.AdminCmd)
	RootCmd.AddCommand(user.UserCmd)
	RootCmd.AddCommand(setting.SettingCmd)
	RootCmd.AddCommand(root.RootCmd)
}
