use super::errors::HandshakeError;

pub trait THandshakeServer {
    fn read_c0(&mut self) -> Result<(), HandshakeError>;
    fn read_c1(&mut self) -> Result<(), HandshakeError>;
    fn read_c2(&mut self) -> Result<(), HandshakeError>;

    fn write_s0(&mut self) -> Result<(), HandshakeError>;
    fn write_s1(&mut self) -> Result<(), HandshakeError>;
    fn write_s2(&mut self) -> Result<(), HandshakeError>;
}

pub trait THandshakeClient {
    fn write_c0(&mut self) -> Result<(), HandshakeError>;
    fn write_c1(&mut self) -> Result<(), HandshakeError>;
    fn write_c2(&mut self) -> Result<(), HandshakeError>;

    fn read_s0(&mut self) -> Result<(), HandshakeError>;
    fn read_s1(&mut self) -> Result<(), HandshakeError>;
    fn read_s2(&mut self) -> Result<(), HandshakeError>;
}
