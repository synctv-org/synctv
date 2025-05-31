package user

import (
	"errors"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var DeleteCmd = &cobra.Command{
	Use:   "delete",
	Short: "delete",
	Long:  `delete user`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, args []string) error {
		if len(args) == 0 {
			return errors.New("missing user id")
		}
		u, err := db.LoadAndDeleteUserByID(args[0])
		if err != nil {
			log.Errorf("delete user failed: %s\n", err)
			return nil
		}
		log.Infof("delete user success: %s\n", u.Username)
		return nil
	},
}

func init() {
	UserCmd.AddCommand(DeleteCmd)
}
