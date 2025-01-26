package user

import (
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var BanCmd = &cobra.Command{
	Use:   "ban",
	Short: "ban user with user id",
	Long:  "ban user with user id",
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
		u, err := db.GetUserByID(args[0])
		if err != nil {
			fmt.Printf("get user failed: %s\n", err)
			return nil
		}
		err = db.BanUser(u)
		if err != nil {
			fmt.Printf("ban user failed: %s\n", err)
			return nil
		}
		fmt.Printf("ban user success: %s\n", u.Username)
		return nil
	},
}

func init() {
	UserCmd.AddCommand(BanCmd)
}
