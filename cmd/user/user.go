package user

import "github.com/spf13/cobra"

var UserCmd = &cobra.Command{
	Use:   "user",
	Short: "user",
	Long:  `you must first shut down the server, otherwise the changes will not take effect.`,
}
