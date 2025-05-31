package admin

import (
	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var ShowCmd = &cobra.Command{
	Use:   "show",
	Short: "show admin",
	Long:  `show admin`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, _ []string) error {
		admins, err := db.GetAdmins()
		if err != nil {
			log.Errorf("get admins failed: %s\n", err.Error())
		}
		for _, admin := range admins {
			log.Infof("id: %s\tusername: %s\n", admin.ID, admin.Username)
		}
		return nil
	},
}

func init() {
	AdminCmd.AddCommand(ShowCmd)
}
