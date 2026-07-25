use axum::{
    extract::State,
    http::{header, HeaderValue},
    response::IntoResponse,
    Json,
};
use serde::Serialize;

use super::AppState;

const ASSOCIATION_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("public, max-age=300");
const ANDROID_LOGIN_CREDENTIAL_RELATION: &str = "delegate_permission/common.get_login_creds";

#[derive(Debug, Serialize)]
struct AppleAppSiteAssociation {
    webcredentials: AppleWebCredentials,
}

#[derive(Debug, Serialize)]
struct AppleWebCredentials {
    apps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AndroidAssetLinkStatement {
    relation: [&'static str; 1],
    target: AndroidAssetLinkTarget,
}

#[derive(Debug, Serialize)]
struct AndroidAssetLinkTarget {
    namespace: &'static str,
    package_name: String,
    sha256_cert_fingerprints: Vec<String>,
}

pub async fn apple_app_site_association(State(state): State<AppState>) -> impl IntoResponse {
    let document = AppleAppSiteAssociation {
        webcredentials: AppleWebCredentials {
            apps: state.runtime_settings.server.apple_app_ids.clone(),
        },
    };
    (
        [(header::CACHE_CONTROL, ASSOCIATION_CACHE_CONTROL)],
        Json(document),
    )
}

pub async fn android_asset_links(State(state): State<AppState>) -> impl IntoResponse {
    let statements = state
        .runtime_settings
        .server
        .android_apps
        .iter()
        .map(|app| AndroidAssetLinkStatement {
            relation: [ANDROID_LOGIN_CREDENTIAL_RELATION],
            target: AndroidAssetLinkTarget {
                namespace: "android_app",
                package_name: app.package_name.clone(),
                sha256_cert_fingerprints: app
                    .sha256_cert_fingerprints
                    .iter()
                    .map(|fingerprint| canonical_sha256_fingerprint(fingerprint))
                    .collect(),
            },
        })
        .collect::<Vec<_>>();
    (
        [(header::CACHE_CONTROL, ASSOCIATION_CACHE_CONTROL)],
        Json(statements),
    )
}

fn canonical_sha256_fingerprint(value: &str) -> String {
    let compact = value
        .chars()
        .filter(char::is_ascii_hexdigit)
        .flat_map(char::to_uppercase)
        .collect::<Vec<_>>();
    compact
        .chunks(2)
        .map(|chunk| chunk.iter().collect::<String>())
        .collect::<Vec<_>>()
        .join(":")
}

#[cfg(test)]
mod tests {
    use super::canonical_sha256_fingerprint;

    #[test]
    fn certificate_fingerprint_is_served_in_android_canonical_form() {
        assert_eq!(
            canonical_sha256_fingerprint(
                "aabbccddeeff00112233445566778899aabbccddeeff00112233445566778899"
            ),
            "AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99:AA:BB:CC:DD:EE:FF:00:11:22:33:44:55:66:77:88:99"
        );
    }
}
