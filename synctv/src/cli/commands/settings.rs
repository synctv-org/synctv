use super::prelude::*;

#[derive(Debug, Args)]
pub struct SettingsCommand {
    #[command(subcommand)]
    pub command: SettingsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsSubcommand {
    /// List effective runtime settings grouped by category
    List(SettingsListArgs),
    /// Get one effective settings group
    Get(SettingsGetArgs),
    /// Update one settings group using repeated --set key=value entries
    Update(SettingsUpdateArgs),
    /// Send a test email using the current runtime email settings
    TestEmail(SettingsTestEmailArgs),
}

#[derive(Debug, Args)]
pub struct SettingsListArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsGetArgs {
    pub group: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsUpdateArgs {
    pub group: String,

    /// Setting override in key=value form. Repeat for multiple fields.
    #[arg(long = "set", value_name = "KEY=VALUE", required = true, num_args = 1..)]
    pub entries: Vec<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsTestEmailArgs {
    pub to: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
