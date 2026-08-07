use std::marker::PhantomData;

use serde::Serialize;

use crate::Result;

pub(crate) struct JsonbArray<T> {
    values: Vec<serde_json::Value>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Serialize> JsonbArray<T> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    pub(crate) fn push(&mut self, value: &T) -> Result<()> {
        self.values.push(serde_json::to_value(value)?);
        Ok(())
    }

    pub(crate) fn as_slice(&self) -> &[serde_json::Value] {
        &self.values
    }
}

pub(crate) struct OptionalJsonbArray<T> {
    values: Vec<Option<serde_json::Value>>,
    _marker: PhantomData<fn() -> T>,
}

impl<T: Serialize> OptionalJsonbArray<T> {
    pub(crate) fn with_capacity(capacity: usize) -> Self {
        Self {
            values: Vec::with_capacity(capacity),
            _marker: PhantomData,
        }
    }

    pub(crate) fn push(&mut self, value: Option<&T>) -> Result<()> {
        self.values
            .push(value.map(serde_json::to_value).transpose()?);
        Ok(())
    }

    pub(crate) fn as_slice(&self) -> &[Option<serde_json::Value>] {
        &self.values
    }
}
