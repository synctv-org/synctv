mod args;
mod commands;
mod completion;
mod context;
mod execute;
mod human_output;
mod output;
mod output_dto;

pub use args::*;
pub use commands::*;
pub use completion::{CompletionArgs, CompletionShell};
pub use execute::{execute, version_string};
pub use output::{ConfigOutputFormat, RemoteOutputFormat};

#[cfg(test)]
pub(in crate::cli) use completion::write_completion_output;
#[cfg(test)]
pub(in crate::cli) use context::{CliConfigContext, RemoteCliContext};
#[cfg(test)]
pub(in crate::cli) use execute::{
    apply_root_global_overrides, batch_user_refs_to_proto, build_get_playback_cli_output,
    database_summary, format_management_status_error, management_stream_item,
    management_unary_response_with_timeout, normalized_provider_types, parse_cli_json,
    parse_masked_settings_request, resolve_remote_endpoint, room_settings_patch_to_full_settings,
    stop_stream_disconnect_can_be_treated_as_success, stop_stream_end_can_be_treated_as_success,
    switch_process_working_dir_to_data_dir, synthesize_stop_completion_if_needed,
    DatabaseCliOutput, StopServerEventOutput, StopServerOutput,
};
#[cfg(test)]
pub(in crate::cli) use human_output::render_human_output;
#[cfg(test)]
pub(in crate::cli) use output::{
    config_json_for_display, redact_config_for_display as render_config_for_display,
};
#[cfg(test)]
mod tests;
