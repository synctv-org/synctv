package user

import (
	"errors"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var UnbanCmd = &cobra.Command{
	Use:   "unban",
	Short: "unban user with user id",
	Long:  "unban user with user id",
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
		u, err := db.GetUserByID(args[0])
		if err != nil {
			log.Errorf("get user failed: %s\n", err)
			return nil
		}
		err = db.UnbanUser(u)
		if err != nil {
			log.Errorf("unban user failed: %s", err)
			return nil
		}
		log.Infof("unban user success: %s\n", u.Username)
		return nil
	},
}

func init() {
	UserCmd.AddCommand(UnbanCmd)
}
