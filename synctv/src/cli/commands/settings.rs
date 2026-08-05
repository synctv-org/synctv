use super::prelude::*;
use std::path::PathBuf;

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
    /// Export a versioned runtime settings backup
    Export(SettingsExportArgs),
    /// Validate and import a runtime settings backup
    Import(SettingsImportArgs),
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
pub struct SettingsExportArgs {
    /// Write the JSON backup to this file; stdout is used when omitted.
    #[arg(long, value_name = "FILE")]
    pub file: Option<PathBuf>,

    /// Atomically replace an existing output file.
    #[arg(long, requires = "file")]
    pub force: bool,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsImportArgs {
    /// Runtime settings backup JSON file, or '-' to read stdin.
    #[arg(value_name = "FILE")]
    pub file: PathBuf,

    /// Validate and report changes without writing settings.
    #[arg(long)]
    pub dry_run: bool,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}

#[derive(Debug, Args)]
pub struct SettingsTestEmailArgs {
    pub to: String,

    #[command(flatten)]
    pub remote: RemoteAccessArgs,
}
