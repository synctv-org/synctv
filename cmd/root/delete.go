package root

import (
	"errors"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var RemoveCmd = &cobra.Command{
	Use:   "remove",
	Short: "remove",
	Long:  `remove root`,
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
			log.Errorf("get user failed: %s", err)
			return nil
		}
		if err := db.RemoveRoot(u); err != nil {
			log.Errorf("remove root failed: %s", err)
			return nil
		}
		log.Infof("remove root success: %s\n", u.Username)
		return nil
	},
}

func init() {
	RootCmd.AddCommand(RemoveCmd)
}
