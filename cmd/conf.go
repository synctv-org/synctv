package cmd

import "github.com/spf13/cobra"

func ConfCmd() *cobra.Command {
	return &cobra.Command{
		Use:   "conf",
		Short: "conf",
		Long:  `config file`,
	}
}
