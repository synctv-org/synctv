use super::prelude::*;

#[derive(Debug, Args)]
pub struct SettingsCommand {
    #[command(subcommand)]
    pub command: SettingsSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum SettingsSubcommand {
    /// Show effective admin settings
    List(SettingsListArgs),
    /// Get one effective settings section
    Get(SettingsGetArgs),
    /// Update runtime settings using an UpdateSettingsRequest ProtoJSON document
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
    /// UpdateSettingsRequest encoded as ProtoJSON.
    #[arg(long = "request-json", value_name = "JSON", required = true)]
    pub request_json: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsTestEmailArgs {
    pub to: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
