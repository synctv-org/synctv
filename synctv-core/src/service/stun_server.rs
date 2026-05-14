//! Built-in STUN server.
//!
//! Provides a minimal RFC 5389 UDP Binding service for server-reflexive
//! candidate discovery. The server is stateless and safe to run on multiple
//! replicas behind the same advertised address.

use std::net::{IpAddr, SocketAddr};
use std::sync::Arc;
use tokio::net::UdpSocket;
use tokio::task::JoinHandle;

const STUN_HEADER_LEN: usize = 20;
const STUN_BINDING_REQUEST: u16 = 0x0001;
const STUN_BINDING_SUCCESS_RESPONSE: u16 = 0x0101;
const STUN_XOR_MAPPED_ADDRESS: u16 = 0x0020;
const STUN_MAGIC_COOKIE: u32 = 0x2112_A442;

#[derive(Debug, Clone)]
pub struct StunServerConfig {
    pub bind_addr: String,
    /// Public address advertised to clients for reaching this STUN server.
    /// This is distinct from the client source address echoed in STUN binding
    /// responses.
    pub external_addr: String,
}

impl Default for StunServerConfig {
    fn default() -> Self {
        Self {
            bind_addr: "0.0.0.0:3478".to_string(),
            external_addr: "0.0.0.0:3478".to_string(),
        }
    }
}

const fn is_unusable_external_ip(ip: &IpAddr) -> bool {
    if ip.is_unspecified() || ip.is_loopback() {
        return true;
    }
    match ip {
        IpAddr::V4(v4) => v4.is_private() || v4.is_link_local(),
        IpAddr::V6(_) => false,
    }
}

pub async fn resolve_external_ip() -> Option<IpAddr> {
    if let Some(ip) = ip_from_env("STUN_EXTERNAL_IP") {
        tracing::info!(ip = %ip, "Resolved external IP from STUN_EXTERNAL_IP env var");
        return Some(ip);
    }

    let Ok(client) = reqwest::Client::builder()
        .timeout(std::time::Duration::from_secs(2))
        .connect_timeout(std::time::Duration::from_secs(1))
        .build()
    else {
        return None;
    };

    if let Some(ip) = try_aws_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from AWS EC2 metadata");
        return Some(ip);
    }

    if let Some(ip) = try_gcp_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from GCP metadata");
        return Some(ip);
    }

    if let Some(ip) = try_azure_metadata(&client).await {
        tracing::info!(ip = %ip, "Resolved external IP from Azure IMDS");
        return Some(ip);
    }

    None
}

fn ip_from_env(var: &str) -> Option<IpAddr> {
    std::env::var(var)
        .ok()
        .and_then(|s| s.trim().parse::<IpAddr>().ok())
        .filter(|ip| !ip.is_unspecified())
}

async fn try_aws_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    let token = client
        .put("http://169.254.169.254/latest/api/token")
        .header("X-aws-ec2-metadata-token-ttl-seconds", "30")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    let ip_str = client
        .get("http://169.254.169.254/latest/meta-data/public-ipv4")
        .header("X-aws-ec2-metadata-token", token.trim())
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<IpAddr>().ok()
}

async fn try_gcp_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    let ip_str = client
        .get("http://metadata.google.internal/computeMetadata/v1/instance/network-interfaces/0/access-configs/0/external-ip")
        .header("Metadata-Flavor", "Google")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    ip_str.trim().parse::<IpAddr>().ok()
}

async fn try_azure_metadata(client: &reqwest::Client) -> Option<IpAddr> {
    let resp = client
        .get("http://169.254.169.254/metadata/instance/network/interface/0/ipv4/ipAddress/0/publicIpAddress?api-version=2021-02-01&format=text")
        .header("Metadata", "true")
        .send()
        .await
        .ok()?
        .text()
        .await
        .ok()?;

    resp.trim().parse::<IpAddr>().ok()
}

pub fn validate_external_addr(addr: &str) -> Result<(), String> {
    let sock: SocketAddr = addr
        .parse()
        .map_err(|e| format!("Invalid STUN external address '{addr}': {e}"))?;

    if is_unusable_external_ip(&sock.ip()) {
        return Err(format!(
            "STUN external address '{addr}' is not routable (unspecified, loopback, or private). \
             Set SYNCTV_WEBRTC_STUN_EXTERNAL_ADDR to a public IP or DNS name, \
             or set STUN_EXTERNAL_IP to a public IP. In Kubernetes, use a LoadBalancer IP, \
             a node public IP, or a public DNS name; do not use a Pod IP or ClusterIP Service IP."
        ));
    }

    Ok(())
}

fn parse_binding_request(packet: &[u8]) -> Option<[u8; 12]> {
    if packet.len() < STUN_HEADER_LEN {
        return None;
    }

    let message_type = u16::from_be_bytes([packet[0], packet[1]]);
    if message_type != STUN_BINDING_REQUEST {
        return None;
    }

    let message_len = usize::from(u16::from_be_bytes([packet[2], packet[3]]));
    if packet.len() < STUN_HEADER_LEN + message_len {
        return None;
    }

    let magic_cookie = u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]);
    if magic_cookie != STUN_MAGIC_COOKIE {
        return None;
    }

    let mut transaction_id = [0_u8; 12];
    transaction_id.copy_from_slice(&packet[8..20]);
    Some(transaction_id)
}

fn encode_xor_mapped_address(transaction_id: [u8; 12], mapped_addr: SocketAddr) -> Vec<u8> {
    let mut value = Vec::with_capacity(match mapped_addr {
        SocketAddr::V4(_) => 8,
        SocketAddr::V6(_) => 20,
    });
    value.push(0);

    let cookie_bytes = STUN_MAGIC_COOKIE.to_be_bytes();
    let xored_port = mapped_addr.port() ^ u16::from_be_bytes([cookie_bytes[0], cookie_bytes[1]]);

    match mapped_addr.ip() {
        IpAddr::V4(ip) => {
            value.push(0x01);
            value.extend_from_slice(&xored_port.to_be_bytes());
            for (octet, mask) in ip.octets().iter().zip(cookie_bytes.iter()) {
                value.push(octet ^ mask);
            }
        }
        IpAddr::V6(ip) => {
            value.push(0x02);
            value.extend_from_slice(&xored_port.to_be_bytes());
            let mut mask = [0_u8; 16];
            mask[..4].copy_from_slice(&cookie_bytes);
            mask[4..].copy_from_slice(&transaction_id);
            for (octet, mask) in ip.octets().iter().zip(mask.iter()) {
                value.push(octet ^ mask);
            }
        }
    }

    value
}

fn build_binding_success_response(transaction_id: [u8; 12], mapped_addr: SocketAddr) -> Vec<u8> {
    let attr_value = encode_xor_mapped_address(transaction_id, mapped_addr);
    let attr_len = u16::try_from(attr_value.len()).expect("xor mapped address length fits in u16");
    let mut response = Vec::with_capacity(STUN_HEADER_LEN + 4 + attr_value.len());

    response.extend_from_slice(&STUN_BINDING_SUCCESS_RESPONSE.to_be_bytes());
    response.extend_from_slice(&(4_u16 + attr_len).to_be_bytes());
    response.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
    response.extend_from_slice(&transaction_id);
    response.extend_from_slice(&STUN_XOR_MAPPED_ADDRESS.to_be_bytes());
    response.extend_from_slice(&attr_len.to_be_bytes());
    response.extend_from_slice(&attr_value);

    response
}

fn build_binding_response(packet: &[u8], mapped_addr: SocketAddr) -> Option<Vec<u8>> {
    parse_binding_request(packet)
        .map(|transaction_id| build_binding_success_response(transaction_id, mapped_addr))
}

pub struct StunServer {
    task: JoinHandle<()>,
    local_addr: SocketAddr,
    external_addr: SocketAddr,
}

impl StunServer {
    pub fn start(config: &StunServerConfig) -> anyhow::Result<Arc<Self>> {
        let listen: SocketAddr = config
            .bind_addr
            .parse()
            .map_err(|e| anyhow::anyhow!("Invalid bind_addr '{}': {e}", config.bind_addr))?;
        let external: SocketAddr = config.external_addr.parse().map_err(|e| {
            anyhow::anyhow!("Invalid external_addr '{}': {e}", config.external_addr)
        })?;

        let std_socket = std::net::UdpSocket::bind(listen)
            .map_err(|e| anyhow::anyhow!("Failed to bind STUN UDP socket on {listen}: {e}"))?;
        std_socket.set_nonblocking(true).map_err(|e| {
            anyhow::anyhow!("Failed to enable nonblocking mode for STUN socket: {e}")
        })?;
        let socket = UdpSocket::from_std(std_socket)
            .map_err(|e| anyhow::anyhow!("Failed to create async STUN socket: {e}"))?;
        let local_addr = socket
            .local_addr()
            .map_err(|e| anyhow::anyhow!("Failed to inspect STUN local address: {e}"))?;

        let task = crate::spawn::spawn_monitored("stun_server", async move {
            let mut buf = [0_u8; 1500];
            loop {
                let (len, peer_addr) = match socket.recv_from(&mut buf).await {
                    Ok(values) => values,
                    Err(error) => {
                        tracing::error!(error = %error, "STUN server receive loop failed");
                        break;
                    }
                };

                let packet = &buf[..len];
                let Some(response) = build_binding_response(packet, peer_addr) else {
                    continue;
                };

                if let Err(error) = socket.send_to(&response, peer_addr).await {
                    tracing::warn!(error = %error, %peer_addr, "STUN server failed to send response");
                }
            }
        });

        tracing::info!(
            bind_addr = %local_addr,
            external_addr = %external,
            "STUN server started"
        );

        Ok(Arc::new(Self {
            task,
            local_addr,
            external_addr: external,
        }))
    }

    #[must_use]
    pub const fn local_addr(&self) -> SocketAddr {
        self.local_addr
    }

    #[must_use]
    pub const fn external_addr(&self) -> SocketAddr {
        self.external_addr
    }

    pub fn shutdown(&self) {
        self.task.abort();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::UdpSocket;
    use tokio::time::{timeout, Duration};

    fn binding_request(transaction_id: [u8; 12]) -> Vec<u8> {
        let mut request = Vec::from(STUN_BINDING_REQUEST.to_be_bytes());
        request.extend_from_slice(&0_u16.to_be_bytes());
        request.extend_from_slice(&STUN_MAGIC_COOKIE.to_be_bytes());
        request.extend_from_slice(&transaction_id);
        request
    }

    fn decode_xor_mapped_address(response: &[u8]) -> SocketAddr {
        assert_eq!(
            u16::from_be_bytes([response[0], response[1]]),
            STUN_BINDING_SUCCESS_RESPONSE
        );
        assert_eq!(
            u32::from_be_bytes([response[4], response[5], response[6], response[7]]),
            STUN_MAGIC_COOKIE
        );

        let attr_type = u16::from_be_bytes([response[20], response[21]]);
        assert_eq!(attr_type, STUN_XOR_MAPPED_ADDRESS);

        let attr_len = usize::from(u16::from_be_bytes([response[22], response[23]]));
        let value = &response[24..24 + attr_len];
        let family = value[1];
        let cookie_bytes = STUN_MAGIC_COOKIE.to_be_bytes();
        let port = u16::from_be_bytes([value[2], value[3]])
            ^ u16::from_be_bytes([cookie_bytes[0], cookie_bytes[1]]);

        match family {
            0x01 => {
                let mut octets = [0_u8; 4];
                for (index, octet) in octets.iter_mut().enumerate() {
                    *octet = value[4 + index] ^ cookie_bytes[index];
                }
                SocketAddr::from((octets, port))
            }
            0x02 => {
                let mut transaction_id = [0_u8; 12];
                transaction_id.copy_from_slice(&response[8..20]);
                let mut mask = [0_u8; 16];
                mask[..4].copy_from_slice(&cookie_bytes);
                mask[4..].copy_from_slice(&transaction_id);
                let mut octets = [0_u8; 16];
                for (index, octet) in octets.iter_mut().enumerate() {
                    *octet = value[4 + index] ^ mask[index];
                }
                SocketAddr::from((octets, port))
            }
            other => panic!("unexpected address family {other}"),
        }
    }

    #[test]
    fn build_binding_response_returns_ipv4_xor_mapped_address() {
        let transaction_id = [1, 35, 69, 103, 137, 171, 205, 239, 2, 4, 6, 8];
        let request = binding_request(transaction_id);
        let mapped_addr = "203.0.113.7:3478"
            .parse()
            .expect("socket addr should parse");

        let response =
            build_binding_response(&request, mapped_addr).expect("binding request should respond");

        assert_eq!(response[8..20], transaction_id);
        assert_eq!(decode_xor_mapped_address(&response), mapped_addr);
    }

    #[test]
    fn build_binding_response_returns_ipv6_xor_mapped_address() {
        let transaction_id = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11];
        let request = binding_request(transaction_id);
        let mapped_addr = "[2001:db8::1234]:3478"
            .parse()
            .expect("socket addr should parse");

        let response =
            build_binding_response(&request, mapped_addr).expect("binding request should respond");

        assert_eq!(decode_xor_mapped_address(&response), mapped_addr);
    }

    #[test]
    fn build_binding_response_rejects_non_stun_packets() {
        assert!(build_binding_response(b"not-stun", "203.0.113.7:3478".parse().unwrap()).is_none());
    }

    #[test]
    fn test_validate_external_addr_public_ip() {
        assert!(validate_external_addr("203.0.113.1:3478").is_ok());
    }

    #[test]
    fn test_validate_external_addr_unspecified() {
        let err = validate_external_addr("0.0.0.0:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_loopback() {
        let err = validate_external_addr("127.0.0.1:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_private_rfc1918() {
        assert!(validate_external_addr("10.0.0.1:3478")
            .unwrap_err()
            .contains("not routable"));
        assert!(validate_external_addr("172.16.0.1:3478")
            .unwrap_err()
            .contains("not routable"));
        assert!(validate_external_addr("192.168.1.1:3478")
            .unwrap_err()
            .contains("not routable"));
    }

    #[test]
    fn test_validate_external_addr_link_local() {
        let err = validate_external_addr("169.254.1.1:3478").unwrap_err();
        assert!(err.contains("not routable"));
    }

    #[tokio::test]
    async fn stun_server_returns_observed_client_address_in_binding_response() {
        let server = StunServer::start(&StunServerConfig {
            bind_addr: "127.0.0.1:0".to_string(),
            external_addr: "203.0.113.1:3478".to_string(),
        })
        .expect("stun server should start");

        let client = UdpSocket::bind("127.0.0.1:0")
            .await
            .expect("client socket should bind");
        let client_addr = client
            .local_addr()
            .expect("client local address should be available");

        let transaction_id = [9, 8, 7, 6, 5, 4, 3, 2, 1, 0, 11, 12];
        let request = binding_request(transaction_id);
        client
            .send_to(&request, server.local_addr())
            .await
            .expect("binding request should send");

        let mut buf = [0_u8; 1500];
        let (len, _) = timeout(Duration::from_secs(2), client.recv_from(&mut buf))
            .await
            .expect("stun response should arrive before timeout")
            .expect("stun response should be readable");

        let response = &buf[..len];
        assert_eq!(response[8..20], transaction_id);
        assert_eq!(decode_xor_mapped_address(response), client_addr);
        assert_ne!(decode_xor_mapped_address(response), server.external_addr());

        server.shutdown();
    }
}
