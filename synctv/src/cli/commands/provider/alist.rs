use super::super::prelude::*;
use super::{
    ProviderBoundCredentialArgs, ProviderCredentialCommandArgs, ProviderServiceInstanceArgs,
    ProviderServiceRemoteActorArgs,
};

#[derive(Debug, Args)]
pub struct ProviderAlistCommand {
    #[command(subcommand)]
    pub command: ProviderAlistSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderAlistSubcommand {
    /// Log a user into Alist and persist the credential bind
    Login(ProviderAlistLoginArgs),
    /// List directory contents using a saved Alist bind
    List(ProviderAlistListArgs),
    /// Search files and directories using a saved Alist bind
    Search(ProviderAlistSearchArgs),
    /// Show the current Alist account info for a saved bind
    Me(ProviderAlistGetMeArgs),
    /// Remove a saved Alist bind
    Logout(ProviderCredentialCommandArgs),
    /// List saved Alist binds for a user
    Binds(ProviderAlistBindsArgs),
}

#[derive(Debug, Args)]
#[command(group(
    ArgGroup::new("alist_login_credential")
        .args(["password", "hashed_password"])
        .required(true)
        .multiple(false)
))]
pub struct ProviderAlistLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Alist server base URL
    #[arg(long)]
    pub server_endpoint: String,

    /// Alist account username used for the remote login
    #[arg(long = "account-username", value_name = "ACCOUNT_USERNAME")]
    pub account_username: String,

    /// Plaintext Alist password. Prefer --hashed-password when available.
    #[arg(long, group = "alist_login_credential")]
    pub password: Option<String>,

    /// Pre-hashed Alist password accepted by the Alist login API
    #[arg(long, group = "alist_login_credential")]
    pub hashed_password: Option<String>,

    /// Current Alist TOTP/2FA code. This is not persisted.
    #[arg(long = "otp-code")]
    pub otp_code: Option<String>,

    /// Alist TOTP secret used to generate future 2FA codes for automatic token refresh.
    #[arg(long = "otp-secret")]
    pub otp_secret: Option<String>,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistListArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Directory path to list. Use `/` for the root.
    #[arg(long)]
    pub path: String,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: u64,

    #[arg(long, default_value_t = 50)]
    pub per_page: u64,

    #[arg(long, default_value_t = false)]
    pub refresh: bool,
}

#[derive(Debug, Args)]
pub struct ProviderAlistSearchArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,

    /// Parent directory path to search under. Use `/` for the root.
    #[arg(long)]
    pub parent: String,

    /// Search keywords.
    #[arg(long)]
    pub keywords: String,

    /// Search scope: 0 = all, 1 = directories, 2 = files.
    #[arg(long, default_value_t = 0)]
    pub scope: u64,

    /// Optional Alist directory password
    #[arg(long)]
    pub password: Option<String>,

    #[arg(long, default_value_t = 1)]
    pub page: u64,

    #[arg(long, default_value_t = 50)]
    pub per_page: u64,
}

#[derive(Debug, Args)]
pub struct ProviderAlistGetMeArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Args)]
pub struct ProviderAlistBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

pub(in crate::cli) fn alist_login_credential(
    args: &ProviderAlistLoginArgs,
) -> Result<synctv_proto::providers::alist::login_request::Credential> {
    if let Some(password) = &args.password {
        return Ok(
            synctv_proto::providers::alist::login_request::Credential::Password(password.clone()),
        );
    }
    if let Some(hashed_password) = &args.hashed_password {
        return Ok(
            synctv_proto::providers::alist::login_request::Credential::HashedPassword(
                hashed_password.clone(),
            ),
        );
    }
    bail!("Alist login requires --password or --hashed-password")
}
