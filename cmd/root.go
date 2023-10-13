package cmd

import (
	"fmt"
	"os"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/version"
)

var RootCmd = &cobra.Command{
	Use:   "synctv-server",
	Short: "synctv-server",
	Long:  `synctv-server https://github.com/synctv-org/synctv`,
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
	RootCmd.PersistentFlags().StringVarP(&flags.ConfigFile, "config", "f", "", "config file path")
	RootCmd.PersistentFlags().StringVar(&flags.GitHubBaseURL, "github-base-url", "https://api.github.com/", "github api base url")
}
