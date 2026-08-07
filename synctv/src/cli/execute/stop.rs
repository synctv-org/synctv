use super::*;

pub(super) async fn execute_stop(args: StopArgs) -> Result<()> {
    let session = connect_remote_access(&args.remote).await?;
    let mut client = session.management_client();
    let mut stream = management_unary_response(
        "stop server",
        client.stop_server(management_proto::StopServerRequest {
            mode: if args.force {
                management_proto::ShutdownMode::Force as i32
            } else {
                management_proto::ShutdownMode::Graceful as i32
            },
        }),
    )
    .await?;

    let mut events = Vec::new();
    let mut saw_terminal = false;
    let mut last_stage = None;
    loop {
        match management_stream_item(
            "stop server",
            MANAGEMENT_STOP_STREAM_IDLE_TIMEOUT,
            stream.message(),
        )
        .await
        {
            Ok(Some(event)) => {
                let stage = management_proto::StopServerStage::try_from(event.stage).ok();
                let message = event.message.trim().to_string();
                if args.remote.output == RemoteOutputFormat::Human && !message.is_empty() {
                    println!("{message}");
                }
                events.push(StopServerEventOutput {
                    stage: event.stage,
                    message,
                    terminal: event.terminal,
                });
                last_stage = stage;
                if event.terminal {
                    saw_terminal = true;
                    break;
                }
            }
            Ok(None) => {
                synthesize_stop_completion_if_needed(
                    args.remote.output,
                    &mut last_stage,
                    &mut saw_terminal,
                    &mut events,
                );
                if stop_stream_end_can_be_treated_as_success(last_stage) {
                    print_stop_output(
                        args.remote.output,
                        &StopServerOutput {
                            success: true,
                            terminal_received: saw_terminal,
                            final_stage: last_stage.map(i32::from),
                            events,
                        },
                    )?;
                    return Ok(());
                }
                break;
            }
            Err(error) => {
                synthesize_stop_completion_if_needed(
                    args.remote.output,
                    &mut last_stage,
                    &mut saw_terminal,
                    &mut events,
                );
                if stop_stream_disconnect_can_be_treated_as_success(last_stage, &error) {
                    print_stop_output(
                        args.remote.output,
                        &StopServerOutput {
                            success: true,
                            terminal_received: saw_terminal,
                            final_stage: last_stage.map(i32::from),
                            events,
                        },
                    )?;
                    return Ok(());
                }
                return Err(error);
            }
        }
    }

    synthesize_stop_completion_if_needed(
        args.remote.output,
        &mut last_stage,
        &mut saw_terminal,
        &mut events,
    );

    if !saw_terminal {
        bail!("management stop stream ended before terminal shutdown status")
    }

    print_stop_output(
        args.remote.output,
        &StopServerOutput {
            success: true,
            terminal_received: saw_terminal,
            final_stage: last_stage.map(i32::from),
            events,
        },
    )?;

    Ok(())
}

pub(in crate::cli) const fn stop_stream_end_can_be_treated_as_success(
    last_stage: Option<management_proto::StopServerStage>,
) -> bool {
    matches!(
        last_stage,
        Some(management_proto::StopServerStage::Finalizing)
    )
}

pub(in crate::cli) fn synthesize_stop_completion_if_needed(
    format: RemoteOutputFormat,
    last_stage: &mut Option<management_proto::StopServerStage>,
    saw_terminal: &mut bool,
    events: &mut Vec<StopServerEventOutput>,
) {
    if !matches!(
        *last_stage,
        Some(management_proto::StopServerStage::Finalizing)
    ) {
        return;
    }

    if format == RemoteOutputFormat::Human {
        println!("shutdown complete");
    }

    events.push(StopServerEventOutput {
        stage: management_proto::StopServerStage::Completed as i32,
        message: "shutdown complete".to_string(),
        terminal: true,
    });
    *last_stage = Some(management_proto::StopServerStage::Completed);
    *saw_terminal = true;
}

pub(in crate::cli) fn stop_stream_disconnect_can_be_treated_as_success(
    last_stage: Option<management_proto::StopServerStage>,
    error: &anyhow::Error,
) -> bool {
    if !stop_stream_end_can_be_treated_as_success(last_stage) {
        return false;
    }

    let message = error.to_string().to_ascii_lowercase();
    message.contains("broken pipe")
        || message.contains("connection closed")
        || message.contains("error reading a body from connection")
        || message.contains("transport error")
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct StopServerEventOutput {
    pub(in crate::cli) stage: i32,
    pub(in crate::cli) message: String,
    pub(in crate::cli) terminal: bool,
}

#[derive(Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub(in crate::cli) struct StopServerOutput {
    pub(in crate::cli) success: bool,
    pub(in crate::cli) terminal_received: bool,
    pub(in crate::cli) final_stage: Option<i32>,
    pub(in crate::cli) events: Vec<StopServerEventOutput>,
}

fn print_stop_output(format: RemoteOutputFormat, output: &StopServerOutput) -> Result<()> {
    match format {
        RemoteOutputFormat::Human => Ok(()),
        RemoteOutputFormat::Json => print_json(output),
        RemoteOutputFormat::Yaml => print_yaml(output),
    }
}
