package cmd

import (
	"fmt"
	"runtime"
	"runtime/debug"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/version"
)

var VersionCmd = &cobra.Command{
	Use:   "version",
	Short: "Print the version number of Sync TV Server",
	Long:  `All software has versions. This is Sync TV Server's`,
	Run: func(_ *cobra.Command, _ []string) {
		fmt.Printf("synctv %s\n", version.Version)
		fmt.Printf("- git/commit: %s\n", version.GitCommit)
		fmt.Printf("- os/platform: %s\n", runtime.GOOS)
		fmt.Printf("- os/arch: %s\n", runtime.GOARCH)
		fmt.Printf("- go/version: %s\n", runtime.Version())
		fmt.Printf("- go/compiler: %s\n", runtime.Compiler)
		fmt.Printf("- go/numcpu: %d\n", runtime.NumCPU())
		info, ok := debug.ReadBuildInfo()
		if ok {
			fmt.Printf("- go/buildsettings: %v\n", info.Settings)
		}
	},
}

func init() {
	RootCmd.AddCommand(VersionCmd)
}
