use super::prelude::*;

#[derive(Debug, Args)]
pub struct DbCommand {
    #[command(flatten)]
    pub global: GlobalConfigArgs,

    #[command(subcommand)]
    pub command: DbSubcommand,
}

#[derive(Debug, Subcommand)]
pub enum DbSubcommand {
    /// Run startup migrations without starting the server
    Migrate(DbMigrateArgs),
    /// Check connectivity and report migration readiness
    Status(DbStatusArgs),
}

#[derive(Debug, Args)]
pub struct DbMigrateArgs {
    /// Output format for migration result
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}

#[derive(Debug, Args)]
pub struct DbStatusArgs {
    /// Output format for database status
    #[arg(long, short = 'o', value_enum, default_value_t = RemoteOutputFormat::Human)]
    pub output: RemoteOutputFormat,
}
