use super::*;

pub(super) async fn execute_serve(args: ServeArgs) -> Result<()> {
    let context = CliConfigContext::new(args.global.clone());
    let config = context.strict_validated_config()?;
    switch_process_working_dir_to_data_dir(&config)?;

    crate::install_panic_hook(config.logging.backtrace);
    let _log_guard = synctv_core::logging::init_logging(&config.logging)?;

    if args.dry_run {
        tracing::info!("Configuration and logging initialized successfully");
        tracing::info!("Dry run requested, not starting server");
        return Ok(());
    }

    tracing::info!("SyncTV server starting...");
    tracing::info!("API address: {}", config.api_address());

    let app = Box::pin(Application::build(config)).await?;
    Box::pin(app.run()).await
}

pub(in crate::cli) fn switch_process_working_dir_to_data_dir(
    config: &synctv_core::Config,
) -> Result<()> {
    let data_dir = PathBuf::from(config.data_dir.trim());

    std::fs::create_dir_all(&data_dir).with_context(|| {
        format!(
            "failed to create data_dir {} before switching working directory",
            absolute_display_path(&data_dir)
        )
    })?;

    std::env::set_current_dir(&data_dir).with_context(|| {
        format!(
            "failed to switch working directory to data_dir {}",
            absolute_display_path(&data_dir)
        )
    })?;

    Ok(())
}

pub(super) fn local_api_probe_address(config: &synctv_core::Config) -> String {
    let host = match config.server.host.trim() {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "::1".to_string(),
        host if host.contains(':') && !host.starts_with('[') => format!("[{host}]"),
        host => host.to_string(),
    };
    format!("{host}:{}", config.server.port)
}
