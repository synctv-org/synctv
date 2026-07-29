use axum::{
    extract::State,
    http::{header, HeaderMap, HeaderValue},
    response::IntoResponse,
    Json,
};
use serde::Serialize;

use super::{AppResult, AppState};

const ASSOCIATION_CACHE_CONTROL: HeaderValue = HeaderValue::from_static("public, max-age=300");
const ANDROID_LOGIN_CREDENTIAL_RELATION: &str = "delegate_permission/common.get_login_creds";
const ANDROID_APP_LINK_RELATION: &str = "delegate_permission/common.handle_all_urls";

#[derive(Debug, Serialize)]
struct AppleAppSiteAssociation {
    applinks: AppleAppLinks,
    webcredentials: AppleWebCredentials,
}

#[derive(Debug, Serialize)]
struct AppleAppLinks {
    apps: Vec<String>,
    details: Vec<AppleAppLinkDetails>,
}

#[derive(Debug, Serialize)]
struct AppleAppLinkDetails {
    #[serde(rename = "appID")]
    app_id: String,
    paths: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AppleWebCredentials {
    apps: Vec<String>,
}

#[derive(Debug, Serialize)]
struct AndroidAssetLinkStatement {
    relation: [&'static str; 2],
    target: AndroidAssetLinkTarget,
}

#[derive(Debug, Serialize)]
struct AndroidAssetLinkTarget {
    namespace: &'static str,
    package_name: String,
    sha256_cert_fingerprints: Vec<String>,
}

pub async fn apple_app_site_association(
    State(state): State<AppState>,
    headers: HeaderMap,
) -> AppResult<impl IntoResponse> {
    let app_ids = state.runtime_settings.server.apple_app_ids.clone();
    let allowed_redirect_urls = match &state.runtime_settings_store {
        Some(settings) => settings.oauth2.allowed_redirect_urls.get()?.0,
        None => Vec::new(),
    };
    let host = headers
        .get(header::HOST)
        .and_then(|value| value.to_str().ok())
        .unwrap_or_default();
    let document = apple_app_site_association_document(
        app_ids,
        &oauth2_app_link_paths(&allowed_redirect_urls, host),
    );
    Ok((
        [(header::CACHE_CONTROL, ASSOCIATION_CACHE_CONTROL)],
        Json(document),
    ))
}

fn apple_app_site_association_document(
    app_ids: Vec<String>,
    callback_paths: &[String],
) -> AppleAppSiteAssociation {
    AppleAppSiteAssociation {
        applinks: AppleAppLinks {
            apps: Vec::new(),
            details: app_ids
                .iter()
                .map(|app_id| AppleAppLinkDetails {
                    app_id: app_id.clone(),
                    paths: callback_paths.to_vec(),
                })
                .collect(),
        },
        webcredentials: AppleWebCredentials { apps: app_ids },
    }
}

fn oauth2_app_link_paths(allowed_redirect_urls: &[String], request_host: &str) -> Vec<String> {
    let Ok(origin) = url::Url::parse(&format!("https://{request_host}")) else {
        return Vec::new();
    };
    let Some(request_host) = origin.host_str() else {
        return Vec::new();
    };

    allowed_redirect_urls
        .iter()
        .filter_map(|value| url::Url::parse(value).ok())
        .filter(|url| {
            url.scheme() == "https"
                && url
                    .host_str()
                    .is_some_and(|host| host.eq_ignore_ascii_case(request_host))
        })
        .map(|url| url.path().to_string())
        .collect::<std::collections::BTreeSet<_>>()
        .into_iter()
        .collect()
}

pub async fn android_asset_links(State(state): State<AppState>) -> impl IntoResponse {
    let statements = state
        .runtime_settings
        .server
        .android_apps
        .iter()
        .map(|app| AndroidAssetLinkStatement {
            relation: [ANDROID_LOGIN_CREDENTIAL_RELATION, ANDROID_APP_LINK_RELATION],
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
    use super::{
        apple_app_site_association_document, canonical_sha256_fingerprint, oauth2_app_link_paths,
    };

    #[test]
    fn apple_document_uses_runtime_oauth_callback_paths() {
        let paths = oauth2_app_link_paths(
            &[
                "https://syncs.tv/oauth2/callback".to_string(),
                "https://syncs.tv/oauth2/callback".to_string(),
                "https://other.example/oauth2/other".to_string(),
                "http://127.0.0.1:34567/oauth2/callback".to_string(),
            ],
            "syncs.tv",
        );
        let document = apple_app_site_association_document(
            vec!["85KBWFQ6F6.org.synctv.app".to_string()],
            &paths,
        );
        assert_eq!(
            serde_json::to_value(document).expect("association document should serialize"),
            serde_json::json!({
                "applinks": {
                    "apps": [],
                    "details": [{
                        "appID": "85KBWFQ6F6.org.synctv.app",
                        "paths": ["/oauth2/callback"]
                    }]
                },
                "webcredentials": {"apps": ["85KBWFQ6F6.org.synctv.app"]}
            })
        );
    }

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
