use super::super::prelude::*;

#[derive(Debug, Clone, Args)]
pub struct ProviderServiceInstanceArgs {
    /// Explicit provider instance name. Omit to use the default backend for that provider type.
    #[arg(long = "instance-name")]
    pub instance_name: Option<String>,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderServiceRemoteActorArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    #[command(flatten)]
    pub actor: ActorUserArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderBoundCredentialArgs {
    /// Stored provider credential server identifier
    #[arg(long)]
    pub server_id: String,

    #[command(flatten)]
    pub instance: ProviderServiceInstanceArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderBoundCredentialInstanceCommandArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    #[command(flatten)]
    pub bind: ProviderBoundCredentialArgs,
}

#[derive(Debug, Clone, Args)]
pub struct ProviderCredentialCommandArgs {
    #[command(flatten)]
    pub access: ProviderServiceRemoteActorArgs,

    /// Stored provider credential server identifier
    #[arg(long)]
    pub server_id: String,
}
