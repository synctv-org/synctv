package admin

import (
	"errors"

	log "github.com/sirupsen/logrus"
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
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, args []string) error {
		if len(args) == 0 {
			return ErrMissingUserID
		}
		u, err := db.GetUserByID(args[0])
		if err != nil {
			log.Errorf("get user failed: %s", err)
			return nil
		}
		if err := db.AddAdmin(u); err != nil {
			log.Errorf("add admin failed: %s", err)
			return nil
		}
		log.Infof("add admin success: %s\n", u.Username)
		return nil
	},
}

func init() {
	AdminCmd.AddCommand(AddCmd)
}
