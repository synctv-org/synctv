use sha2::{Digest, Sha256};
use subtle::ConstantTimeEq;
use tonic::{Request, Status};

#[derive(Clone, Debug, Default)]
pub(crate) struct ManagementAccessController {
    required_bearer_token_digest: Option<[u8; 32]>,
}

impl ManagementAccessController {
    pub(crate) fn new(auth_token: &str) -> Self {
        let trimmed = auth_token.trim();
        let required_bearer_token_digest = (!trimmed.is_empty()).then(|| token_digest(trimmed));
        Self {
            required_bearer_token_digest,
        }
    }

    pub(crate) fn authorize<T: std::fmt::Debug>(&self, request: &Request<T>) -> Result<(), Status> {
        let Some(expected_token_digest) = &self.required_bearer_token_digest else {
            return Ok(());
        };

        let header_value = request
            .metadata()
            .get("authorization")
            .ok_or_else(|| Status::unauthenticated("Management authentication required"))?
            .to_str()
            .map_err(|_| Status::unauthenticated("Invalid management authorization header"))?;

        let provided_token = synctv_core::service::JwtValidator::extract_bearer_token(header_value)
            .map_err(|_| Status::unauthenticated("Invalid management authorization header"))?;

        if constant_time_eq(&token_digest(&provided_token), expected_token_digest) {
            Ok(())
        } else {
            Err(Status::unauthenticated(
                "Invalid management authorization header",
            ))
        }
    }
}

fn token_digest(token: &str) -> [u8; 32] {
    let digest = Sha256::digest(token.as_bytes());
    let mut bytes = [0; 32];
    bytes.copy_from_slice(&digest);
    bytes
}

fn constant_time_eq(left: &[u8; 32], right: &[u8; 32]) -> bool {
    left.ct_eq(right).into()
}

#[cfg(test)]
mod tests {
    use super::ManagementAccessController;
    use tonic::{metadata::MetadataValue, Code, Request, Status};

    #[test]
    fn management_access_controller_allows_missing_header_when_token_disabled() -> Result<(), Status>
    {
        let controller = ManagementAccessController::new("");
        let request = Request::new(());

        controller.authorize(&request)?;
        Ok(())
    }

    #[test]
    fn management_access_controller_rejects_missing_header_when_token_configured() {
        let controller = ManagementAccessController::new("management-secret");
        let request = Request::new(());

        let error = controller
            .authorize(&request)
            .expect_err("missing auth header must be rejected when management token is configured");

        assert_eq!(error.code(), Code::Unauthenticated);
        assert_eq!(error.message(), "Management authentication required");
    }

    #[test]
    fn management_access_controller_rejects_incorrect_bearer_token() {
        let controller = ManagementAccessController::new("management-secret");
        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::from_static("Bearer wrong-secret"),
        );

        let error = controller
            .authorize(&request)
            .expect_err("wrong management bearer token must be rejected");

        assert_eq!(error.code(), Code::Unauthenticated);
        assert_eq!(error.message(), "Invalid management authorization header");
    }

    #[test]
    fn management_access_controller_accepts_matching_bearer_token() -> Result<(), Status> {
        let controller = ManagementAccessController::new("management-secret");
        let mut request = Request::new(());
        request.metadata_mut().insert(
            "authorization",
            MetadataValue::from_static("Bearer management-secret"),
        );

        controller.authorize(&request)?;
        Ok(())
    }
}
