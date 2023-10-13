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
	RunE:  Conf,
}

func Conf(cmd *cobra.Command, args []string) error {
	err := bootstrap.InitConfig(cmd.Context())
	if err != nil {
		return err
	}
	fmt.Println(conf.Conf.String())
	return nil
}

func init() {
	RootCmd.AddCommand(ConfCmd)
}
