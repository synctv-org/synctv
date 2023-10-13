package cmd

import (
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/conf"
)

var ConfCmd = &cobra.Command{
	Use:   "conf",
	Short: "conf",
	Long:  `config file`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitConfig,
		).Run()
	},
	RunE: Conf,
}

func Conf(cmd *cobra.Command, args []string) error {
	fmt.Println(conf.Conf.String())
	return nil
}

func init() {
	RootCmd.AddCommand(ConfCmd)
}
