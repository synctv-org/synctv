package setting

import "github.com/spf13/cobra"

var SettingCmd = &cobra.Command{
	Use:   "setting",
	Short: "setting",
	Long:  `you must first shut down the server, otherwise the changes will not take effect.`,
}
