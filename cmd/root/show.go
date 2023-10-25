package root

import (
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var ShowCmd = &cobra.Command{
	Use:   "show",
	Short: "show root",
	Long:  `show root`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		roots := db.GetRoots()
		for _, root := range roots {
			fmt.Printf("id: %d\tusername: %s\n", root.ID, root.Username)
		}
		return nil
	},
}

func init() {
	RootCmd.AddCommand(ShowCmd)
}
