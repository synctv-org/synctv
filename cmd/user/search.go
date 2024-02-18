package user

import (
	"errors"
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var SearchCmd = &cobra.Command{
	Use:   "search",
	Short: "search user by id or username",
	Long:  `search user by id or username`,
	PreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		if len(args) == 0 {
			return errors.New("missing user id or username")
		}
		us, err := db.GetUserByIDOrUsernameLike(args[0])
		if err != nil {
			return err
		}
		if len(us) == 0 {
			fmt.Println("user not found")
			return nil
		}
		for _, u := range us {
			fmt.Printf("id: %s\tusername: %s\tcreated_at: %s\trole: %s\n", u.ID, u.Username, u.CreatedAt, u.Role)
		}
		return nil
	},
}

func init() {
	UserCmd.AddCommand(SearchCmd)
}
