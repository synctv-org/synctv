//! HTTP extraction helpers.
//!
//! Transport handlers deserialize inputs and hand requests to `impls`; request
//! validation belongs in the impl/core layers shared by HTTP and gRPC.

use axum::{
    extract::{rejection::QueryRejection, FromRequestParts, Query},
    http::request::Parts,
};
use serde::de::DeserializeOwned;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtoQuery<T>(pub T);

impl<T> std::ops::Deref for ProtoQuery<T> {
    type Target = T;

    fn deref(&self) -> &Self::Target {
        &self.0
    }
}

impl<T> std::ops::DerefMut for ProtoQuery<T> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.0
    }
}

fn map_query_rejection(rejection: &QueryRejection) -> super::AppError {
    super::AppError::new(rejection.status(), rejection.body_text())
}

impl<S, T> FromRequestParts<S> for ProtoQuery<T>
where
    S: Send + Sync,
    T: DeserializeOwned + Send,
{
    type Rejection = super::AppError;

    async fn from_request_parts(parts: &mut Parts, state: &S) -> Result<Self, Self::Rejection> {
        let Query(value) = Query::<T>::from_request_parts(parts, state)
            .await
            .map_err(|rejection| map_query_rejection(&rejection))?;
        Ok(Self(value))
    }
}
