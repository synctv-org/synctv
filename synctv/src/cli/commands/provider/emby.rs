use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderEmbyCommand {
    #[command(subcommand)]
    pub command: ProviderEmbySubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderEmbySubcommand {
    /// Log a user into Emby/Jellyfin and persist the credential bind
    Login(ProviderEmbyLoginArgs),
    /// List Emby-compatible library items using a saved bind
    List(ProviderEmbyListArgs),
    /// Show the current Emby-compatible account info for a saved bind
    Me(ProviderEmbyGetMeArgs),
    /// Remove a saved Emby-compatible bind
    Logout(ProviderCredentialCommandArgs),
    /// List saved Emby-compatible binds for a user
    Binds(ProviderEmbyBindsArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("emby_login_credential")
        .args(["password", "no_password", "api_key"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderEmbyLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Emby/Jellyfin server base URL
    #[arg(long)]
    pub server_endpoint: String,

    /// Target Emby/Jellyfin account username to bind
    #[arg(long)]
    pub account_username: String,

    /// Emby/Jellyfin account password. Conflicts with --no-password and --api-key.
    #[arg(long, group = "emby_login_credential")]
    pub password: Option<String>,

    /// Authenticate an Emby/Jellyfin account that has no password.
    #[arg(long, group = "emby_login_credential")]
    pub no_password: bool,

    /// Emby/Jellyfin API key. Conflicts with --password and --no-password.
    #[arg(long, group = "emby_login_credential")]
    pub api_key: Option<String>,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

pub(in crate::cli) fn emby_login_credential(
    args: &ProviderEmbyLoginArgs,
) -> Result<synctv_proto::providers::emby::login_request::Credential> {
    if let Some(password) = &args.password {
        return Ok(
            synctv_proto::providers::emby::login_request::Credential::Password(password.clone()),
        );
    }
    if args.no_password {
        return Ok(
            synctv_proto::providers::emby::login_request::Credential::Password(String::new()),
        );
    }
    if let Some(api_key) = &args.api_key {
        return Ok(
            synctv_proto::providers::emby::login_request::Credential::ApiKey(api_key.clone()),
        );
    }
    bail!("Emby login requires --password, --no-password, or --api-key")
}

#[derive(Debug, Args)]
pub struct ProviderEmbyListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Library path or parent item identifier to list. Use an empty string for the root if needed.
    #[arg(long)]
    pub path: String,

    #[arg(long, default_value_t = 0)]
    pub start_index: u64,

    #[arg(long, default_value_t = 50)]
    pub limit: u64,

    /// Optional fuzzy search term applied by the provider backend
    #[arg(long)]
    pub search_term: Option<String>,
}

#[derive(Debug, Args)]
pub struct ProviderEmbyGetMeArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderEmbyBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
