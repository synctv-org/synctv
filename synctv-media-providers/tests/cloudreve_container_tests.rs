//! Cloudreve v4 contract checks against the official container image.

use anyhow::{ensure, Context};
use serde_json::{json, Value};
use synctv_core_testing::{start_external_service, ExternalServiceRequest};
use synctv_media_providers::CloudreveClient;

const CLOUDREVE_IMAGE: &str = "cloudreve/cloudreve";
const CLOUDREVE_TAG: &str = "4.17.0";
const CLOUDREVE_PORT: u16 = 5212;
const TEST_EMAIL: &str = "admin@example.com";
const TEST_PASSWORD: &str = "test-password";

async fn post_json(
    http: &reqwest::Client,
    url: &str,
    body: Value,
    bearer_token: Option<&str>,
) -> anyhow::Result<Value> {
    let mut request = http.post(url).json(&body);
    if let Some(token) = bearer_token {
        request = request.bearer_auth(token);
    }
    let response = request.send().await?.error_for_status()?;
    Ok(response.json().await?)
}

fn ensure_cloudreve_success(response: &Value, operation: &str) -> anyhow::Result<()> {
    let code = response["code"]
        .as_i64()
        .with_context(|| format!("Cloudreve {operation} response has no numeric code"))?;
    ensure!(
        code == 0,
        "Cloudreve {operation} failed with code {code}: {}",
        response["msg"].as_str().unwrap_or_default()
    );
    Ok(())
}

#[tokio::test]
#[ignore = "requires Docker and pulls cloudreve/cloudreve:4.17.0"]
async fn cloudreve_v4_container_matches_login_user_and_list_contracts() -> anyhow::Result<()> {
    let container = start_external_service(
        ExternalServiceRequest::new(
            "cloudreve",
            "synctv-cloudreve-",
            CLOUDREVE_IMAGE,
            CLOUDREVE_TAG,
            CLOUDREVE_PORT,
        )
        .with_stdout_ready_message("Listening to \":5212\""),
    )
    .await;
    let base_url = container.http_url();
    let http = reqwest::Client::new();

    let registration = post_json(
        &http,
        &format!("{base_url}/api/v4/user"),
        json!({
            "email": TEST_EMAIL,
            "password": TEST_PASSWORD,
            "language": "en-US"
        }),
        None,
    )
    .await?;
    ensure_cloudreve_success(&registration, "registration")?;

    let client = CloudreveClient::with_http_client(&base_url, http.clone())?;
    let token = client.login(TEST_EMAIL, TEST_PASSWORD).await?;
    ensure!(
        !token.access_token.is_empty(),
        "access token should be present"
    );
    ensure!(
        !token.refresh_token.is_empty(),
        "refresh token should be present"
    );

    let user = client.me(&token.access_token).await?;
    ensure!(
        user.email == TEST_EMAIL,
        "current user email should round trip"
    );
    ensure!(!user.id.is_empty(), "current user ID should be present");

    let create_folder = post_json(
        &http,
        &format!("{base_url}/api/v4/file/create"),
        json!({
            "uri": "cloudreve://my/Contract",
            "type": "folder",
            "metadata": {},
            "err_on_conflict": true
        }),
        Some(&token.access_token),
    )
    .await?;
    ensure_cloudreve_success(&create_folder, "folder creation")?;

    let listing = client.list(&token.access_token, "", 1, None, 20).await?;
    let folder = listing
        .files
        .iter()
        .find(|item| item.name == "Contract")
        .context("created Cloudreve folder should appear in the root listing")?;
    ensure!(
        folder.is_dir(),
        "created Cloudreve item should be a directory"
    );
    ensure!(
        folder.path == "cloudreve://my/Contract",
        "Cloudreve URI should round trip"
    );

    Ok(())
}
