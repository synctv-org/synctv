package setting

import (
	"fmt"

	"github.com/spf13/cobra"
	"github.com/synctv-org/synctv/internal/bootstrap"
	"github.com/synctv-org/synctv/internal/settings"
)

var FixCmd = &cobra.Command{
	Use:   "fix",
	Short: "fix setting",
	Long:  `fix setting`,
	PersistentPreRunE: func(cmd *cobra.Command, args []string) error {
		return bootstrap.New(bootstrap.WithContext(cmd.Context())).Add(
			bootstrap.InitDiscardLog,
			bootstrap.InitConfig,
			bootstrap.InitDatabase,
			bootstrap.InitSetting,
		).Run()
	},
	RunE: func(cmd *cobra.Command, args []string) error {
		count := 0
		errorCount := 0
		for k, s := range settings.Settings {
			_, err := s.Interface()
			if err != nil {
				fmt.Printf("setting %s, interface error: %v\n", k, err)
				err = s.SetRaw(s.DefaultRaw())
				if err != nil {
					errorCount++
					fmt.Printf("setting %s fix error: %v\n", k, err)
				} else {
					count++
					fmt.Printf("setting %s fix success\n", k)
				}
			}
		}
		fmt.Printf("fix success: %d, fix error: %d\n", count, errorCount)
		return nil
	},
}

func init() {
	SettingCmd.AddCommand(FixCmd)
}
