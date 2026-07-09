use http::{HeaderMap, HeaderValue};

pub fn extract_client_ip<T>(
    request: &tonic::Request<T>,
    is_trusted_proxy: impl Fn(&std::net::IpAddr) -> bool,
) -> Result<Option<std::net::IpAddr>, tonic::Status> {
    let remote_addr = request
        .extensions()
        .get::<axum::extract::ConnectInfo<std::net::SocketAddr>>()
        .map(|info| info.0.ip())
        .or_else(|| {
            request
                .extensions()
                .get::<tonic::transport::server::TcpConnectInfo>()
                .and_then(tonic::transport::server::TcpConnectInfo::remote_addr)
                .map(|addr| addr.ip())
        });

    if let Some(peer_ip) = remote_addr {
        let headers = forwarded_ip_headers(request.metadata())?;
        return crate::client_ip::extract_client_ip_from_headers(
            is_trusted_proxy,
            peer_ip,
            &headers,
        )
        .map(Some)
        .map_err(|error| tonic::Status::invalid_argument(error.to_string()));
    }

    Ok(remote_addr)
}

fn forwarded_ip_headers(
    metadata: &tonic::metadata::MetadataMap,
) -> Result<HeaderMap, tonic::Status> {
    let mut headers = HeaderMap::new();
    for header_name in ["x-forwarded-for", "x-real-ip"] {
        if let Some(value) = metadata.get(header_name) {
            let value = value
                .to_str()
                .map_err(|_| {
                    tonic::Status::invalid_argument(format!(
                        "{header_name} metadata must be valid ASCII"
                    ))
                })?
                .parse::<HeaderValue>()
                .map_err(|_| {
                    tonic::Status::invalid_argument(format!(
                        "{header_name} metadata must be a valid HTTP header value"
                    ))
                })?;
            headers.insert(header_name, value);
        }
    }
    Ok(headers)
}
