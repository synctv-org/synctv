package cmd

import (
	"fmt"
	"runtime"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/conf"
)

var VersionCmd = &cobra.Command{
	Use:   "version",
	Short: "Print the version number of Sync TV Server",
	Long:  `All software has versions. This is Sync TV Server's`,
	Run: func(cmd *cobra.Command, args []string) {
		fmt.Printf("Sync TV Server version %s\n", conf.Version)
		fmt.Printf("Sync TV Web version %s\n", conf.WebVersion)
		fmt.Printf("Git commit %s\n", conf.GitCommit)
		fmt.Printf("Go version %s\n", runtime.Version())
		fmt.Printf("Built with %s\n", runtime.Compiler)
		fmt.Printf("OS/Arch %s/%s\n", runtime.GOOS, runtime.GOARCH)
	},
}

func init() {
	RootCmd.AddCommand(VersionCmd)
}
