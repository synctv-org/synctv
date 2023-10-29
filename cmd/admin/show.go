package admin

import (
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var ShowCmd = &cobra.Command{
	Use:   "show",
	Short: "show admin",
	Long:  `show admin`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		admins := db.GetAdmins()
		for _, admin := range admins {
			fmt.Printf("id: %s\tusername: %s\n", admin.ID, admin.Username)
		}
		return nil
	},
}

func init() {
	AdminCmd.AddCommand(ShowCmd)
}
