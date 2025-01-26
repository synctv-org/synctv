package user

import (
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var DeleteCmd = &cobra.Command{
	Use:   "delete",
	Short: "delete",
	Long:  `delete user`,
	PreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return errors.New("missing user id")
		}
		u, err := db.LoadAndDeleteUserByID(args[0])
		if err != nil {
			fmt.Printf("delete user failed: %s\n", err)
			return nil
		}
		fmt.Printf("delete user success: %s\n", u.Username)
		return nil
	},
}

func init() {
	UserCmd.AddCommand(DeleteCmd)
}
