use synctv_management::runtime_error::RuntimeError;

mod admin;
mod provider_services;
mod providers;

pub(crate) use admin::ManagementAdminRuntime;
pub(crate) use provider_services::{
    ManagementAcfunRuntime, ManagementCctvRuntime, ManagementCloudreveRuntime,
    ManagementDouyuRuntime, ManagementFnosRuntime, ManagementHuyaRuntime,
    ManagementNextcloudRuntime, ManagementQnapRuntime, ManagementSeafileRuntime,
    ManagementSynologyRuntime, ManagementTruenasRuntime, ManagementYoutubeRuntime,
};
pub(crate) use providers::{
    ManagementAlistRuntime, ManagementBilibiliRuntime, ManagementDouyinRuntime,
    ManagementEmbyRuntime, ManagementProviderCommonRuntime, ManagementTikTokRuntime,
    ManagementTwitchRuntime,
};

fn map_runtime_error(error: &impl synctv_adapter::error::ClassifiedError) -> RuntimeError {
    RuntimeError::from_classified_error(error)
}
