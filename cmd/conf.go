package cmd

import (
	"github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
)

var ConfCmd = &cobra.Command{
	Use:   "conf",
	Short: "init or check",
	Long:  `Init or check config file for correctness`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitConfig,
		).Run()
	},
	RunE: Conf,
}

func Conf(cmd *cobra.Command, args []string) error {
	logrus.Infof("success")
	return nil
}

func init() {
	RootCmd.AddCommand(ConfCmd)
}
