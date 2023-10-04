package cmd

import (
	"fmt"

	"github.com/spf13/cobra"
)

var VersionCmd = &cobra.Command{
	Use:   "version",
	Short: "Print the version number of Sync TV Server",
	Long:  `All software has versions. This is Sync TV Server's`,
	Run: func(cmd *cobra.Command, args []string) {
		fmt.Println("synctv-server v0.1 -- HEAD")
	},
}

func init() {
	RootCmd.AddCommand(VersionCmd)
}
