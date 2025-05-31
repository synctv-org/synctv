package root

import (
	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/db"
)

var ShowCmd = &cobra.Command{
	Use:   "show",
	Short: "show root",
	Long:  `show root`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, _ []string) error {
		roots := db.GetRoots()
		for _, root := range roots {
			log.Infof("id: %s\tusername: %s\n", root.ID, root.Username)
		}
		return nil
	},
}

func init() {
	RootCmd.AddCommand(ShowCmd)
}
