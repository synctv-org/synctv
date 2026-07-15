use synctv_management::runtime_error::RuntimeError;

mod admin;
mod providers;

pub(crate) use admin::ManagementAdminRuntime;
pub(crate) use providers::{
    ManagementAlistRuntime, ManagementBilibiliRuntime, ManagementDouyinRuntime,
    ManagementEmbyRuntime, ManagementProviderCommonRuntime, ManagementTikTokRuntime,
    ManagementTwitchRuntime,
};

fn map_runtime_error(error: &impl synctv_adapter::error::ClassifiedError) -> RuntimeError {
    RuntimeError::from_classified_error(error)
}
