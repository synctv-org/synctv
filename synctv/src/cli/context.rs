use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};

use super::args::{GlobalConfigArgs, RemoteAccessArgs};
use super::execute::resolve_remote_endpoint;
use crate::admin_client::AdminConnectionOptions;
use crate::config_loader::{load_config_with_options, load_public_id_config_with_options};

#[derive(Clone)]
pub(super) struct CliConfigContext {
    global: GlobalConfigArgs,
    unvalidated: Arc<OnceLock<std::result::Result<crate::app_config::AppConfig, String>>>,
    validated: Arc<OnceLock<std::result::Result<crate::app_config::AppConfig, String>>>,
}

impl CliConfigContext {
    pub(super) fn new(global: GlobalConfigArgs) -> Self {
        Self {
            global,
            unvalidated: Arc::new(OnceLock::new()),
            validated: Arc::new(OnceLock::new()),
        }
    }

    pub(super) fn config(&self) -> Result<crate::app_config::AppConfig> {
        self.load(false)
    }

    pub(super) fn validated_config(&self) -> Result<crate::app_config::AppConfig> {
        self.load(true)
    }

    pub(super) fn strict_validated_config(&self) -> Result<crate::app_config::AppConfig> {
        self.validated_config()
    }

    pub(super) fn public_id_config(&self) -> Result<synctv_adapter::PublicIdConfig> {
        load_public_id_config_with_options(&self.global.load_options(false))
    }

    fn load(&self, validate: bool) -> Result<crate::app_config::AppConfig> {
        let cache = if validate {
            &self.validated
        } else {
            &self.unvalidated
        };

        match cache.get_or_init(|| {
            load_config_with_options(&self.global.load_options(validate))
                .map_err(|error| error.to_string())
        }) {
            Ok(config) => Ok(config.clone()),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }
}

#[derive(Clone)]
pub(super) struct RemoteCliContext {
    config: CliConfigContext,
    explicit_endpoint: Option<String>,
    resolved_config_endpoint: Arc<OnceLock<std::result::Result<Option<String>, String>>>,
}

impl RemoteCliContext {
    pub(super) fn new(args: &RemoteAccessArgs) -> Self {
        Self {
            config: CliConfigContext::new(args.global.clone()),
            explicit_endpoint: resolve_remote_endpoint(&args.global),
            resolved_config_endpoint: Arc::new(OnceLock::new()),
        }
    }

    pub(super) fn initialize_output_state(&self) -> Result<()> {
        if self.explicit_endpoint.is_none() {
            let _ = self.config.config()?;
        }
        Ok(())
    }

    pub(super) fn connection_options(
        &self,
        args: &RemoteAccessArgs,
    ) -> Result<AdminConnectionOptions> {
        let mut options = args.connection_options(self.explicit_endpoint.clone());
        options.resolved_config_endpoint = self.resolved_config_endpoint()?;
        Ok(options)
    }

    pub(super) fn resolved_config_endpoint(&self) -> Result<Option<String>> {
        match self.resolved_config_endpoint.get_or_init(|| {
            if self.explicit_endpoint.is_some() {
                Ok(None)
            } else {
                let config = self.config.config().map_err(|error| error.to_string())?;
                Ok(Some(config.management_endpoint()))
            }
        }) {
            Ok(endpoint) => Ok(endpoint.clone()),
            Err(error) => Err(anyhow!(error.clone())),
        }
    }
}
