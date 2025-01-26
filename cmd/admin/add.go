package admin

import (
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var ErrMissingUserID = errors.New("missing user id")

var AddCmd = &cobra.Command{
	Use:   "add",
	Short: "add admin by user id",
	Long:  `add admin by user id`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(_ *cobra.Command, args []string) error {
		if len(args) == 0 {
			return ErrMissingUserID
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
