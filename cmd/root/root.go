package root

import "github.com/spf13/cobra"

var RootCmd = &cobra.Command{
	Use:   "root",
	Short: "root",
	Long:  `you must first shut down the server, otherwise the changes will not take effect.`,
}
