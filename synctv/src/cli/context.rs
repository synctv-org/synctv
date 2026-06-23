use std::sync::{Arc, OnceLock};

use anyhow::{anyhow, Result};
use synctv_core::bootstrap::load_config_with_options;

use super::args::{GlobalConfigArgs, RemoteAccessArgs};
use super::execute::resolve_remote_endpoint;
use crate::admin_client::AdminConnectionOptions;

#[derive(Clone)]
pub(super) struct CliConfigContext {
    global: GlobalConfigArgs,
    unvalidated: Arc<OnceLock<std::result::Result<synctv_core::Config, String>>>,
    validated: Arc<OnceLock<std::result::Result<synctv_core::Config, String>>>,
}

impl CliConfigContext {
    pub(super) fn new(global: GlobalConfigArgs) -> Self {
        Self {
            global,
            unvalidated: Arc::new(OnceLock::new()),
            validated: Arc::new(OnceLock::new()),
        }
    }

    pub(super) fn config(&self) -> Result<synctv_core::Config> {
        self.load(false)
    }

    pub(super) fn validated_config(&self) -> Result<synctv_core::Config> {
        self.load(true)
    }

    pub(super) fn strict_validated_config(&self) -> Result<synctv_core::Config> {
        load_config_with_options(&self.global.load_options(true))
    }

    fn load(&self, validate: bool) -> Result<synctv_core::Config> {
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
