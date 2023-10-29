package admin

import (
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var AddCmd = &cobra.Command{
	Use:   "add",
	Short: "add admin by user id",
	Long:  `add admin by user id`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return errors.New("missing user id")
		}
		u, err := db.GetUserByID(args[0])
		if err != nil {
			fmt.Printf("get user failed: %s", err)
			return nil
		}
		if err := db.AddAdmin(u); err != nil {
			fmt.Printf("add admin failed: %s", err)
			return nil
		}
		fmt.Printf("add admin success: %s\n", u.Username)
		return nil
	},
}

func init() {
	AdminCmd.AddCommand(AddCmd)
}
