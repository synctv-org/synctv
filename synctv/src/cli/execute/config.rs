use super::*;

pub(super) fn execute_config(config_command: ConfigCommand) -> Result<()> {
    let context = CliConfigContext::new(config_command.global.clone());
    match config_command.command {
        ConfigSubcommand::Validate(args) => {
            let config = if args.strict {
                context.strict_validated_config()?
            } else {
                context.validated_config()?
            };
            for diagnostic in config.startup_diagnostics() {
                match diagnostic {
                    crate::app_config::StartupDiagnostic::Info(message) => {
                        eprintln!("Info: {message}");
                    }
                    crate::app_config::StartupDiagnostic::Warning(message) => {
                        eprintln!("Warning: {message}");
                    }
                }
            }
            println!("Configuration is valid");
            println!("API address: {}", config.api_address());
            Ok(())
        }
        ConfigSubcommand::Show(args) => {
            let config = context.config()?;
            let rendered = redact_config_for_display(&config)?;
            match args.output {
                ConfigOutputFormat::Yaml => print_yaml(&rendered)?,
                ConfigOutputFormat::Json => {
                    println!(
                        "{}",
                        serde_json::to_string_pretty(&config_json_for_display(&config)?)?
                    );
                }
                ConfigOutputFormat::Toml => print_toml(&rendered)?,
            }
            Ok(())
        }
    }
}
