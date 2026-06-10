use super::bytes_errors::BytesReadError;
use super::bytes_errors::BytesWriteError;

#[derive(Debug, thiserror::Error)]
pub enum BitErrorValue {
    #[error("bytes read error")]
    BytesReadError(BytesReadError),
    #[error("bytes write error")]
    BytesWriteError(BytesWriteError),
    #[error("the size is bigger than 64")]
    TooBig,
    #[error("cannot write the whole 8 bits")]
    CannotWrite8Bit,
    #[error("cannot read byte")]
    CannotReadByte,
}

#[derive(Debug, thiserror::Error)]
#[error("{value}")]
pub struct BitError {
    pub value: BitErrorValue,
}

impl From<BitErrorValue> for BitError {
    fn from(val: BitErrorValue) -> Self {
        Self { value: val }
    }
}

impl From<BytesReadError> for BitError {
    fn from(error: BytesReadError) -> Self {
        Self {
            value: BitErrorValue::BytesReadError(error),
        }
    }
}

impl From<BytesWriteError> for BitError {
    fn from(error: BytesWriteError) -> Self {
        Self {
            value: BitErrorValue::BytesWriteError(error),
        }
    }
}
