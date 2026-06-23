use super::super::prelude::*;
use super::{ProviderServiceInstanceArgs, ProviderServiceRemoteActorArgs};

#[derive(Debug, Args)]
pub struct ProviderBilibiliCommand {
    #[command(subcommand)]
    pub command: ProviderBilibiliSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum ProviderBilibiliSubcommand {
    /// Parse a Bilibili URL, using the user's global bind when available
    Parse(ProviderBilibiliParseArgs),
    /// Generate a QR code for Bilibili login
    LoginQr(ProviderBilibiliLoginQrArgs),
    /// Poll the QR code login status and persist the bind on success
    CheckQr(ProviderBilibiliCheckQrArgs),
    /// Start a Bilibili SMS login session and return Geetest parameters
    StartSmsLogin(ProviderBilibiliStartSmsLoginArgs),
    /// Send a Bilibili SMS verification code
    SendSms(ProviderBilibiliSendSmsArgs),
    /// Log in with Bilibili SMS and persist the bind
    LoginSms(ProviderBilibiliLoginSmsArgs),
    /// Show the current Bilibili account info for the user's global bind
    Me(ProviderBilibiliGetUserInfoArgs),
    /// Remove a saved Bilibili bind
    Logout(ProviderBilibiliLogoutArgs),
    /// List saved Bilibili binds for a user
    Binds(ProviderBilibiliBindsArgs),
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliParseArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// Bilibili page URL to parse
    pub url: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliCheckQrArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,

    /// QR login polling key returned by `login-qr`
    #[arg(long)]
    pub key: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliStartSmsLoginArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliSendSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Mobile phone number in the format expected by the backend provider
    #[arg(long)]
    pub phone: String,

    /// Signed SMS login session token returned by `start-sms-login`
    #[arg(long)]
    pub session_token: String,

    /// Geetest validate result produced by a frontend captcha widget
    #[arg(long)]
    pub validate: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLoginSmsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// SMS verification code
    #[arg(long)]
    pub code: String,

    /// Signed SMS login session token returned by `send-sms`
    #[arg(long)]
    pub session_token: String,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliGetUserInfoArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliLogoutArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Args)]
pub struct ProviderBilibiliBindsArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}
