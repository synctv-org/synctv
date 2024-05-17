package cmd

import (
	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/cmd/flags"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/version"
)

const SelfUpdateLong = `self-update command will update synctv-server binary to latest version.

Version check in: https://github.com/synctv-org/synctv/releases/latest

If use '--dev' flag, will update to latest dev version always.`

var SelfUpdateCmd = &cobra.Command{
	Use:   "self-update",
	Short: "self-update",
	Long:  SelfUpdateLong,
	PreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitStdLog,
		).Run()
	},
	RunE: SelfUpdate,
}

func SelfUpdate(cmd *cobra.Command, args []string) error {
	v, err := version.NewVersionInfo(version.WithBaseURL(flags.Global.GitHubBaseURL))
	if err != nil {
		log.Errorf("get version info error: %v", err)
		return err
	}
	return v.SelfUpdate(cmd.Context())
}

func init() {
	RootCmd.AddCommand(SelfUpdateCmd)
}
