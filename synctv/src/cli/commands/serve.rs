use super::prelude::*;

#[derive(Debug, Args)]
pub struct ServeArgs {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[arg(long, default_value_t = false)]
    pub dry_run: bool,
}
