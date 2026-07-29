use super::*;

pub(super) async fn execute_serve(args: ServeArgs) -> Result<()> {
    let context = CliConfigContext::new(args.global.clone());
    let config = context.validated_config()?;
    let public_id_config = context.public_id_config()?;
    switch_process_working_dir_to_data_dir(&config)?;

    crate::install_panic_hook(config.logging.backtrace);
    let _log_guard =
        synctv_core::logging::init_logging(&crate::resource_options::logging_options(&config))?;

    if args.dry_run {
        tracing::info!("Configuration and logging initialized successfully");
        tracing::info!("Dry run requested, not starting server");
        return Ok(());
    }

    tracing::info!("SyncTV server starting...");
    tracing::info!("API address: {}", config.api_address());

    let app = Box::pin(Application::build_with_options(
        config,
        crate::ApplicationBuildOptions {
            public_id_config,
            ..crate::ApplicationBuildOptions::default()
        },
    ))
    .await?;
    Box::pin(app.run()).await
}

pub(in crate::cli) fn switch_process_working_dir_to_data_dir(
    config: &crate::app_config::AppConfig,
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

pub(super) fn local_api_probe_address(config: &crate::app_config::AppConfig) -> String {
    let host = match config.server.host.trim() {
        "" | "0.0.0.0" => "127.0.0.1".to_string(),
        "::" | "[::]" => "::1".to_string(),
        host if host.contains(':') && !host.starts_with('[') => format!("[{host}]"),
        host => host.to_string(),
    };
    format!("{host}:{}", config.server.port)
}
