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
    /// Update runtime settings
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
    /// Set a settings leaf using PATH=VALUE; may be repeated.
    #[arg(long, value_name = "PATH=VALUE", conflicts_with = "request_json")]
    pub set: Vec<String>,

    /// Unset a settings leaf; may be repeated.
    #[arg(long, value_name = "PATH", conflicts_with = "request_json")]
    pub unset: Vec<String>,

    /// UpdateSettingsRequest encoded as ProtoJSON.
    #[arg(
        long = "request-json",
        value_name = "JSON",
        conflicts_with_all = ["set", "unset"]
    )]
    pub request_json: Option<String>,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsTestEmailArgs {
    pub to: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
