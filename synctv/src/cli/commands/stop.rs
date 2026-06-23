use super::prelude::*;

#[derive(Debug, Args)]
pub struct StopArgs {
    #[command(flatten)]
    pub remote: RemoteAccessArgs,

    /// Request an immediate shutdown path with minimal draining
    #[arg(long, default_value_t = false)]
    pub force: bool,
}
