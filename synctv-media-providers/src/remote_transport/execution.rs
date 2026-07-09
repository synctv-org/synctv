use super::RemoteProviderConnection;
use std::future::Future;

pub(crate) async fn execute_remote_call<T, E, F>(
    connection: &RemoteProviderConnection,
    context: &str,
    future: F,
) -> Result<T, E>
where
    F: Future<Output = Result<T, E>>,
    E: From<crate::ProviderClientError>,
{
    // This is an intentional cancellation boundary around outbound remote I/O.
    // Upper-layer business logic remains cooperatively cancellable; only the
    // remote transport wait itself is aborted here.
    let request_timeout = connection.effective_request_timeout();
    let run = async move {
        tokio::time::timeout(request_timeout, future)
            .await
            .map_err(|_| {
                E::from(crate::ProviderClientError::Network(format!(
                    "remote transport request timeout ({}s) for {context}",
                    request_timeout.as_secs_f64(),
                )))
            })?
    };

    match connection.request_context() {
        Some(request_context) => {
            let cancellation = request_context.cancellation_token();
            tokio::select! {
                () = cancellation.cancelled() => Err(E::from(
                    crate::ProviderClientError::Network(format!(
                        "remote transport request cancelled for {context}"
                    ))
                )),
                result = run => result,
            }
        }
        None => run.await,
    }
}
