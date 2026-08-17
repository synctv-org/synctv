use super::*;

pub(super) async fn execute_db(db_command: DbCommand) -> Result<()> {
    let context = CliConfigContext::new(db_command.global.clone());
    let config = context.validated_config()?;
    let _log_guard =
        synctv_core::logging::init_logging(&crate::resource_options::logging_options(&config))?;
    log_startup_diagnostics(&config);

    match db_command.command {
        DbSubcommand::Migrate(args) => {
            let pool = crate::bootstrap::init_database(
                &crate::resource_options::database_init_options(&config),
            )
            .await?
            .pool;

            crate::migrations::run_migrations(&pool).await?;

            let migrations_status = crate::migrations::inspect_embedded_migrations(&pool).await?;
            let output = DatabaseCliOutput::migrate(&config, &migrations_status);
            print_database_output(args.output, &output)?;
            pool.close().await;
            Ok(())
        }
        DbSubcommand::Status(args) => {
            let pool = crate::bootstrap::init_database(
                &crate::resource_options::database_init_options(&config),
            )
            .await?
            .pool;
            sqlx::query!("SELECT 1 AS ok").fetch_one(&pool).await?;
            let migrations_status = crate::migrations::inspect_embedded_migrations(&pool).await?;
            let output = DatabaseCliOutput::status(&config, &migrations_status);
            print_database_output(args.output, &output)?;
            pool.close().await;
            Ok(())
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum DatabaseCliAction {
    Status,
    Migrate,
}

#[derive(Debug, Clone, Serialize)]
pub(in crate::cli) struct DatabaseCliOutput {
    #[serde(skip_serializing_if = "Option::is_none")]
    database_connection: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration: Option<&'static str>,
    migration_status: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    migration_detail: Option<String>,
    database_url: String,
    #[serde(skip)]
    action: DatabaseCliAction,
}

impl DatabaseCliOutput {
    pub(in crate::cli) fn status(
        config: &crate::app_config::AppConfig,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self::new(DatabaseCliAction::Status, config, migrations_status)
    }

    fn migrate(
        config: &crate::app_config::AppConfig,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self::new(DatabaseCliAction::Migrate, config, migrations_status)
    }

    fn new(
        action: DatabaseCliAction,
        config: &crate::app_config::AppConfig,
        migrations_status: &crate::migrations::EmbeddedMigrationsStatus,
    ) -> Self {
        Self {
            database_connection: (action == DatabaseCliAction::Status).then_some("ok"),
            migration: (action == DatabaseCliAction::Migrate).then_some("completed"),
            migration_status: migrations_status.label(),
            migration_detail: migrations_status.detail(),
            database_url: mask_connection_url(&config.database.url),
            action,
        }
    }

    fn human_header(&self) -> &'static str {
        match self.action {
            DatabaseCliAction::Status => "Database connection: OK",
            DatabaseCliAction::Migrate => "Database migration: completed",
        }
    }
}

fn print_database_output(format: RemoteOutputFormat, output: &DatabaseCliOutput) -> Result<()> {
    match format {
        RemoteOutputFormat::Human => {
            print!("{}", database_summary(output));
            Ok(())
        }
        RemoteOutputFormat::Json => print_json(output),
        RemoteOutputFormat::Yaml => print_yaml(output),
    }
}

pub(in crate::cli) fn database_summary(output: &DatabaseCliOutput) -> String {
    let mut lines = vec![
        output.human_header().to_string(),
        format!("Migration status: {}", output.migration_status),
    ];
    if let Some(detail) = output.migration_detail.as_deref() {
        lines.push(format!("Migration detail: {detail}"));
    }
    lines.push(format!("Database URL: {}", output.database_url));
    lines.push(String::new());
    lines.join("\n")
}
