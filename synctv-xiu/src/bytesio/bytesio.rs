use std::net::SocketAddr;
use std::time::Duration;

use async_trait::async_trait;
use bytes::BufMut;
use bytes::Bytes;
use bytes::BytesMut;
use futures::SinkExt;
use futures::StreamExt;
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
    fn get_net_type(&self) -> NetType;
}

pub struct UdpIO {
    socket: UdpSocket,
}

impl UdpIO {
    pub async fn new(remote_domain: String, remote_port: u16, local_port: u16) -> Option<Self> {
        let remote_address = if remote_domain == "localhost" {
            format!("127.0.0.1:{remote_port}")
        } else {
            format!("{remote_domain}:{remote_port}")
        };
        tracing::info!("remote address: {remote_address}");
        let local_address = format!("0.0.0.0:{local_port}");
        if let Ok(local_socket) = UdpSocket::bind(local_address).await {
            if let Ok(remote_socket_addr) = remote_address.parse::<SocketAddr>() {
                if let Err(err) = local_socket.connect(remote_socket_addr).await {
                    tracing::info!("connect to remote udp socket error: {err}");
                }

                return Some(Self {
                    socket: local_socket,
                });
            }
            tracing::error!("remote_address parse error: {remote_address:?}");
        }

        None
    }

    pub async fn new_with_local_port(local_port: u16) -> Option<Self> {
        let local_address = format!("0.0.0.0:{local_port}");

        if let Ok(local_socket) = UdpSocket::bind(local_address).await {
            return Some(Self {
                socket: local_socket,
            });
        }
        None
    }

    pub fn get_local_port(&self) -> Option<u16> {
        if let Ok(local_addr) = self.socket.local_addr() {
            tracing::info!("local address: {local_addr}");
            return Some(local_addr.port());
        }

        None
    }
}

pub async fn new_udpio_pair() -> Option<(UdpIO, UdpIO)> {
    let mut next_local_port = 0;
    let first_local_port;

    // get the first available port
    if let Some(udpio_0) = UdpIO::new_with_local_port(next_local_port).await {
        if let Some(local_port_0) = udpio_0.get_local_port() {
            first_local_port = local_port_0;
        } else {
            tracing::error!("cannot get local port");
            return None;
        }

        if first_local_port == 65535 {
            next_local_port = 1;
        } else if let Some(udpio_1) = UdpIO::new_with_local_port(first_local_port + 1).await {
            return Some((udpio_0, udpio_1));
        } else if first_local_port + 1 == 65535 {
            next_local_port = 1;
        } else {
            next_local_port = first_local_port + 2;
        }
    } else {
        return None;
    }

    loop {
        tracing::trace!("next local port: {next_local_port} and first port: {first_local_port}");

        if next_local_port == 65535 {
            next_local_port = 1;
            continue;
        }

        if next_local_port == first_local_port {
            return None;
        }

        if let Some(udpio_0) = UdpIO::new_with_local_port(next_local_port).await {
            if let Some(udpio_1) = UdpIO::new_with_local_port(next_local_port + 1).await {
                return Some((udpio_0, udpio_1));
            } else if next_local_port + 1 == 65535 {
                next_local_port = 1;
            } else {
                next_local_port += 2;
            }
        } else {
            // try next port
            next_local_port += 1;
        }
    }
    //None
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
}

pub struct TcpIO {
    stream: Framed<TcpStream, BytesCodec>,
    //timeout: Duration,
}

impl TcpIO {
    pub fn new(stream: TcpStream) -> Self {
        Self {
            stream: Framed::new(stream, BytesCodec::new()),
            // timeout: ms,
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

        match message {
            Some(data) => match data {
                Ok(bytes) => Ok(bytes),
                Err(err) => Err(BytesIOError {
                    value: BytesIOErrorValue::IOError(err),
                }),
            },
            None => Err(BytesIOError {
                value: BytesIOErrorValue::NoneReturn,
            }),
        }
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
        assert!(pair.is_some(), "UDP IO pair creation should succeed");

        let (udpio1, udpio2) = pair.unwrap();
        let port1 = udpio1.get_local_port();
        let port2 = udpio2.get_local_port();

        assert!(port1.is_some(), "First UDP socket should have valid port");
        assert!(port2.is_some(), "Second UDP socket should have valid port");
        assert_ne!(port1, port2, "UDP sockets should have different ports");
    }

    #[tokio::test]
    async fn test_new_udpio_pair2() {
        println!("test_new_udpio_pair2 begin...");
        let mut socket: Vec<UdpIO> = Vec::new();

        for i in 1..=65535 {
            println!("cur port:== {i}");
            //if i % 2 == 1 {
            println!("cur port: {i}");
            if let Some(udpio) = UdpIO::new_with_local_port(i).await {
                socket.push(udpio);
            } else {
                println!("new local port fail: {i}");
            }
            //}
        }

        println!("socket size: {}", socket.len());

        if let Some((udpio1, udpid2)) = new_udpio_pair().await {
            println!(
                "{:?} == {:?}",
                udpio1.get_local_port(),
                udpid2.get_local_port()
            );
        }
    }

    #[tokio::test]
    async fn test_new_udpio_pair3() {
        // get the first available port

        let mut first_local_port = 0;
        if let Some(udpio_0) = UdpIO::new_with_local_port(0).await {
            if let Some(local_port_0) = udpio_0.get_local_port() {
                first_local_port = local_port_0;
            }

            // std::mem::drop(udpio_0);
        }
        //The object udpio_0 is automatically cleared and released when it goes out of scope here.
        println!("first_local_port: {first_local_port}");

        if (UdpIO::new_with_local_port(first_local_port).await).is_some() {
            println!("success");
        } else {
            println!("fail");
        }
    }
}
