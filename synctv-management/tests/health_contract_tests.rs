#![allow(clippy::unwrap_used)]

use tonic::transport::Server;
use tonic_health::pb::health_check_response::ServingStatus;
use tonic_health::pb::health_client::HealthClient;
use tonic_health::pb::HealthCheckRequest;

#[tokio::test]
async fn management_health_reports_empty_service_as_serving() {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0")
        .await
        .expect("health test listener should bind");
    let addr = listener
        .local_addr()
        .expect("health test listener should expose local address");

    let (reporter, health_service) = tonic_health::server::health_reporter();
    reporter
        .set_service_status("", tonic_health::ServingStatus::Serving)
        .await;

    let serve_handle = tokio::spawn(async move {
        Server::builder()
            .add_service(health_service)
            .serve_with_incoming(tokio_stream::wrappers::TcpListenerStream::new(listener))
            .await
            .expect("health-only management server should serve");
    });

    let endpoint = format!("http://{addr}");
    let channel = tonic::transport::Endpoint::from_shared(endpoint)
        .expect("health test endpoint should be valid")
        .connect()
        .await
        .expect("health test channel should connect");

    let mut client = HealthClient::new(channel);
    let response = client
        .check(HealthCheckRequest {
            service: String::new(),
        })
        .await
        .expect("empty-service health check should succeed")
        .into_inner();

    assert_eq!(response.status, ServingStatus::Serving as i32);

    serve_handle.abort();
    let _ = serve_handle.await;
}
