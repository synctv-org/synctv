package user

import (
	"errors"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var BanCmd = &cobra.Command{
	Use:   "ban",
	Short: "ban user with user id",
	Long:  "ban user with user id",
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
		err = db.BanUser(u)
		if err != nil {
			log.Errorf("ban user failed: %s\n", err)
			return nil
		}
		log.Infof("ban user success: %s\n", u.Username)
		return nil
	},
}

func init() {
	UserCmd.AddCommand(BanCmd)
}
