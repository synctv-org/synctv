package cmd

import (
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
)

func Init(cmd *cobra.Command, args []string) error {
	bootstrap.InitConfig()
	bootstrap.InitLog()
	bootstrap.InitSysNotify()
	return nil
}

var InitCmd = &cobra.Command{
	Use:   "init",
	Short: "init and check config",
	Long:  `auto create config file or check config, and auto add new key and delete old key`,
	RunE:  Init,
}

func init() {
	RootCmd.AddCommand(InitCmd)
}
