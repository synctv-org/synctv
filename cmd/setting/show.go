package setting

import (
	"fmt"
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
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
			bootstrap.InitSetting,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		m := make(map[model.SettingGroup]map[string]any)
		for g, s := range settings.GroupSettings {
			if _, ok := m[g]; !ok {
				m[g] = make(map[string]any)
			}
			for _, v := range s {
				i, err := v.Interface()
				if err != nil {
					fmt.Printf("parse setting %s error: %v\ntry to fix setting\n", v.Name(), err)
					return nil
				}
				m[g][v.Name()] = i
			}
		}
		return yaml.NewEncoder(os.Stdout).Encode(m)
	},
}

func init() {
	SettingCmd.AddCommand(ShowCmd)
}
