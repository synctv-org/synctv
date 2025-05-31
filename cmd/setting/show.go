package setting

import (
	"os"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/model"
	"github.com/synctv-org/synctv/internal/settings"
	"gopkg.in/yaml.v3"
)

var ShowCmd = &cobra.Command{
	Use:   "show",
	Short: "show setting",
	Long:  `show setting`,
	PreRunE: func(cmd *cobra.Command, _ []string) error {
		return bootstrap.New().Add(
			bootstrap.InitStdLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
			bootstrap.InitSetting,
		).Run(cmd.Context())
	},
	RunE: func(_ *cobra.Command, _ []string) error {
		m := make(map[model.SettingGroup]map[string]any)
		for g, s := range settings.GroupSettings {
			if _, ok := m[g]; !ok {
				m[g] = make(map[string]any)
			}
			for _, v := range s {
				m[g][v.Name()] = v.Interface()
			}
		}
		return yaml.NewEncoder(os.Stdout).Encode(m)
	},
}

func init() {
	SettingCmd.AddCommand(ShowCmd)
}
