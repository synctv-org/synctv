use super::prelude::*;

#[derive(Debug, Args)]
pub struct ConfigCommand {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[command(subcommand)]
    pub command: ConfigSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ConfigSubcommand {
    /// Validate resolved configuration
    Validate(ConfigValidateArgs),
    /// Print the resolved configuration with secrets redacted
    Show(ConfigShowArgs),
}

#[derive(Debug, Args)]
pub struct ConfigValidateArgs {
    /// Reject unknown config-file keys and unsupported SYNCTV_ environment variables
    #[arg(long, default_value_t = false)]
    pub strict: bool,
}

#[derive(Debug, Args)]
pub struct ConfigShowArgs {
    /// Output format for the rendered configuration
    #[arg(long, short = 'o', value_enum, default_value_t = ConfigOutputFormat::Yaml)]
    pub output: ConfigOutputFormat,
}
