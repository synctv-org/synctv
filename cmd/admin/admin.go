package admin

import (
	"github.com/spf13/cobra"
)

var AdminCmd = &cobra.Command{
	Use:   "admin",
	Short: "admin",
	Long:  `you must first shut down the server, otherwise the changes will not take effect.`,
}
