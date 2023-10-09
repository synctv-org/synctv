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
		fmt.Printf("synctv %s\n", conf.Version)
		fmt.Printf("- web/version: %s\n", conf.WebVersion)
		fmt.Printf("- git/commit: %s\n", conf.GitCommit)
		fmt.Printf("- os/platform: %s\n", runtime.GOOS)
		fmt.Printf("- os/arch: %s\n", runtime.GOARCH)
		fmt.Printf("- go/version: %s\n", runtime.Version())
		fmt.Printf("- go/compiler: %s\n", runtime.Compiler)
	},
}

func init() {
	RootCmd.AddCommand(VersionCmd)
}
