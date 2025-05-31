package setting

import (
	"errors"

	log "github.com/sirupsen/logrus"
	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/settings"
)

var SetCmd = &cobra.Command{
	Use:   "set",
	Short: "set setting",
	Long:  `set setting`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
			bootstrap.InitSetting,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, args []string) error {
		if len(args) != 2 {
			return errors.New("args length must be 2")
		}
		s, ok := settings.Settings[args[0]]
		if !ok {
			return errors.New("setting not found")
		}
		err := s.SetString(args[1])
		if err != nil {
			log.Errorf("set setting %s error: %v\n", args[0], err)
		}
		log.Infof("set setting success:\n%s: %v\n", args[0], s.Interface())
		return nil
	},
}

func init() {
	SettingCmd.AddCommand(SetCmd)
}
