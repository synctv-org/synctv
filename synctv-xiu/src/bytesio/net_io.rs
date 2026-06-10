use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;
use futures::SinkExt;
use futures::StreamExt;
use tokio::io::AsyncWriteExt;
use tokio::net::TcpStream;
use tokio::net::UdpSocket;
use tokio_util::codec::BytesCodec;
use tokio_util::codec::Framed;

use super::bytesio_errors::{BytesIOError, BytesIOErrorValue};

pub enum NetType {
    TCP,
    UDP,
}

#[async_trait]
pub trait TNetIO: Send + Sync {
    async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError>;
    async fn read(&mut self) -> Result<BytesMut, BytesIOError>;
    async fn read_timeout(&mut self, duration: Duration) -> Result<BytesMut, BytesIOError>;
    async fn shutdown(&mut self) -> Result<(), BytesIOError>;
    fn get_net_type(&self) -> NetType;
}

pub struct UdpIO {
    socket: UdpSocket,
}

impl UdpIO {
    pub async fn new(
        remote_domain: String,
        remote_port: u16,
        local_port: u16,
    ) -> Result<Self, BytesIOError> {
        let remote_address = if remote_domain == "localhost" {
            format!("127.0.0.1:{remote_port}")
        } else {
            format!("{remote_domain}:{remote_port}")
        };
        tracing::info!("remote address: {remote_address}");
        let local_address = format!("0.0.0.0:{local_port}");

        let local_socket = UdpSocket::bind(local_address).await?;
        let remote_socket_addr =
            remote_address
                .parse::<SocketAddr>()
                .map_err(|source| BytesIOError {
                    value: BytesIOErrorValue::InvalidSocketAddress {
                        address: remote_address,
                        source,
                    },
                })?;

        local_socket.connect(remote_socket_addr).await?;

        Ok(Self {
            socket: local_socket,
        })
    }

    pub async fn new_with_local_port(local_port: u16) -> Result<Self, BytesIOError> {
        let local_address = format!("0.0.0.0:{local_port}");

        let local_socket = UdpSocket::bind(local_address).await?;
        Ok(Self {
            socket: local_socket,
        })
    }

    pub fn get_local_port(&self) -> Option<u16> {
        if let Ok(local_addr) = self.socket.local_addr() {
            tracing::info!("local address: {local_addr}");
            return Some(local_addr.port());
        }

        None
    }
}

pub async fn new_udpio_pair() -> Result<(UdpIO, UdpIO), BytesIOError> {
    let mut next_local_port = 0;
    let first_local_port;

    {
        let udpio_0 = UdpIO::new_with_local_port(next_local_port).await?;
        if let Some(local_port_0) = udpio_0.get_local_port() {
            first_local_port = local_port_0;
        } else {
            return Err(BytesIOError {
                value: BytesIOErrorValue::NoAvailableUdpPortPair,
            });
        }

        if first_local_port == 65535 {
            next_local_port = 1;
        } else if let Ok(udpio_1) = UdpIO::new_with_local_port(first_local_port + 1).await {
            return Ok((udpio_0, udpio_1));
        } else if first_local_port + 1 == 65535 {
            next_local_port = 1;
        } else {
            next_local_port = first_local_port + 2;
        }
    }

    loop {
        tracing::trace!("next local port: {next_local_port} and first port: {first_local_port}");

        if next_local_port == 65535 {
            next_local_port = 1;
            continue;
        }

        if next_local_port == first_local_port {
            return Err(BytesIOError {
                value: BytesIOErrorValue::NoAvailableUdpPortPair,
            });
        }

        if let Ok(udpio_0) = UdpIO::new_with_local_port(next_local_port).await {
            if let Ok(udpio_1) = UdpIO::new_with_local_port(next_local_port + 1).await {
                return Ok((udpio_0, udpio_1));
            } else if next_local_port + 1 == 65535 {
                next_local_port = 1;
            } else {
                next_local_port += 2;
            }
        } else {
            next_local_port += 1;
        }
    }
}

#[async_trait]
impl TNetIO for UdpIO {
    fn get_net_type(&self) -> NetType {
        NetType::UDP
    }

    async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError> {
        self.socket.send(bytes.as_ref()).await?;
        Ok(())
    }

    async fn read_timeout(&mut self, duration: Duration) -> Result<BytesMut, BytesIOError> {
        match tokio::time::timeout(duration, self.read()).await {
            Ok(data) => data,
            Err(err) => Err(BytesIOError {
                value: BytesIOErrorValue::TimeoutError(err),
            }),
        }
    }

    async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
        let mut buf = vec![0; 4096];
        let len = self.socket.recv(&mut buf).await?;
        let mut rv = BytesMut::new();
        rv.put(&buf[..len]);

        Ok(rv)
    }

    async fn shutdown(&mut self) -> Result<(), BytesIOError> {
        Ok(())
    }
}

pub struct TcpIO {
    stream: Framed<TcpStream, BytesCodec>,
}

impl TcpIO {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream: Framed::new(stream, BytesCodec::new()),
        }
    }
}

#[async_trait]
impl TNetIO for TcpIO {
    fn get_net_type(&self) -> NetType {
        NetType::TCP
    }

    async fn write(&mut self, bytes: Bytes) -> Result<(), BytesIOError> {
        self.stream.send(bytes).await?;

        Ok(())
    }

    async fn read_timeout(&mut self, duration: Duration) -> Result<BytesMut, BytesIOError> {
        match tokio::time::timeout(duration, self.read()).await {
            Ok(data) => data,
            Err(err) => Err(BytesIOError {
                value: BytesIOErrorValue::TimeoutError(err),
            }),
        }
    }

    async fn read(&mut self) -> Result<BytesMut, BytesIOError> {
        let message = self.stream.next().await;

        message.map_or_else(
            || {
                Err(BytesIOError {
                    value: BytesIOErrorValue::ConnectionClosed,
                })
            },
            |data| match data {
                Ok(bytes) => Ok(bytes),
                Err(err) => Err(BytesIOError {
                    value: BytesIOErrorValue::IOError(err),
                }),
            },
        )
    }

    async fn shutdown(&mut self) -> Result<(), BytesIOError> {
        self.stream.get_mut().shutdown().await?;
        Ok(())
    }
}

#[cfg(test)]
mod tests {

    use super::new_udpio_pair;
    use super::UdpIO;

    use tokio;

    #[tokio::test]
    async fn test_new_udpio_pair() {
        let pair = new_udpio_pair().await;
        assert!(pair.is_ok(), "UDP IO pair creation should succeed");

        let (udpio1, udpio2) = pair.expect("UDP IO pair creation should succeed");
        let port1 = udpio1.get_local_port();
        let port2 = udpio2.get_local_port();

        assert!(port1.is_some(), "First UDP socket should have valid port");
        assert!(port2.is_some(), "Second UDP socket should have valid port");
        assert_ne!(port1, port2, "UDP sockets should have different ports");
    }

    #[tokio::test]
    async fn test_new_udpio_pair2() {
        // Bind a handful of known high ports to verify port allocation,
        // then confirm new_udpio_pair still works with ports occupied.
        let mut sockets: Vec<UdpIO> = Vec::new();
        for port in [10000, 10001, 10002, 10003, 10004] {
            if let Ok(udpio) = UdpIO::new_with_local_port(port).await {
                sockets.push(udpio);
            }
        }

        let (udpio1, udpio2) = new_udpio_pair()
            .await
            .expect("UDP IO pair creation should succeed with occupied high ports");
        let port1 = udpio1.get_local_port();
        let port2 = udpio2.get_local_port();
        assert!(port1.is_some(), "First UDP socket should have valid port");
        assert!(port2.is_some(), "Second UDP socket should have valid port");
        assert_ne!(port1, port2, "UDP sockets should have different ports");
    }
}
