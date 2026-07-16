use super::*;

type TestResult = std::result::Result<(), Box<dyn std::error::Error + Send + Sync>>;

fn missing(message: &'static str) -> Box<dyn std::error::Error + Send + Sync> {
    anyhow::anyhow!(message).into()
}

#[test]
fn test_validate_path_normal() {
    assert!(validate_path("/movies/video.mp4").is_ok());
    assert!(validate_path("/").is_ok());
    assert!(validate_path("/a/b/c").is_ok());
    assert!(validate_path("relative/path").is_ok());
}

#[test]
fn test_validate_path_traversal_rejected() {
    assert!(validate_path("/movies/../etc/passwd").is_err());
    assert!(validate_path("..").is_err());
    assert!(validate_path("/..").is_err());
    assert!(validate_path("/../secret").is_err());
    assert!(validate_path("/a/b/../../c").is_err());
}

#[test]
fn test_validate_path_dot_allowed() {
    assert!(validate_path("/movies/.hidden").is_ok());
    assert!(validate_path("/.config/app").is_ok());
}

#[test]
fn test_validate_path_double_encoded_traversal() {
    // %252e%252e -> first decode -> %2e%2e -> second decode -> ..
    assert!(validate_path("/movies/%252e%252e/etc/passwd").is_err());
    assert!(validate_path("/movies/%25252e%25252e/secret").is_err());
}

#[test]
fn test_validate_path_backslash_traversal() {
    assert!(validate_path("/movies/..\\..\\etc\\passwd").is_err());
    assert!(validate_path("/movies/..%5c..%5cetc%5cpasswd").is_err());
    assert!(validate_path("/movies/..%5C..%5Cetc").is_err());
}

#[test]
fn test_validate_path_null_bytes() {
    assert!(validate_path("/movies/video\0.mp4").is_err());
    assert!(validate_path("/movies/video%00.mp4").is_err());
}

#[test]
fn test_validate_path_encoded_traversal_single_layer() {
    assert!(validate_path("/movies/%2e%2e%2fetc%2fpasswd").is_err());
    assert!(validate_path("/movies/%2E%2E%2Fetc").is_err());
}

#[test]
fn test_validate_path_valid_paths_still_pass() {
    assert!(validate_path("/").is_ok());
    assert!(validate_path("/movies/video.mp4").is_ok());
    assert!(validate_path("/a/b/c/d").is_ok());
    assert!(validate_path("/movies/.hidden-file").is_ok());
    assert!(validate_path("/path/with spaces/file.mp4").is_ok());
    assert!(validate_path("/path/file%20name.mp4").is_ok());
}

#[test]
fn test_set_token() -> TestResult {
    let mut client = AlistClient::new("https://alist.example.com")?;
    assert!(!client.has_token());

    client.set_token("new_token");
    assert!(client.has_token());
    Ok(())
}

#[test]
fn test_client_host_preserved() -> TestResult {
    let client = AlistClient::new("https://my-server.com:5244")?;
    assert_eq!(client.host(), "https://my-server.com:5244");
    Ok(())
}

#[test]
fn test_client_host_normalizes_trailing_slash() -> TestResult {
    let client = AlistClient::new("https://my-server.com:5244/base/")?;
    assert_eq!(client.host(), "https://my-server.com:5244/base");
    Ok(())
}

#[test]
fn test_client_host_normalizes_trailing_slash_without_reencoding_path() -> TestResult {
    let client = AlistClient::new("https://proxy.example/a%20list/")?;
    assert_eq!(client.host(), "https://proxy.example/a%20list");
    Ok(())
}

#[test]
fn test_client_with_token_host() -> TestResult {
    let client = AlistClient::with_token("https://alist.example.com", "token123")?;
    assert_eq!(client.host(), "https://alist.example.com");
    assert!(client.has_token());
    Ok(())
}

#[test]
fn test_set_token_overwrite() -> TestResult {
    let mut client = AlistClient::with_token("https://alist.example.com", "old_token")?;
    assert!(client.has_token());
    client.set_token("new_token");
    assert!(client.has_token());
    Ok(())
}

#[test]
fn test_build_headers_uses_origin_without_path() -> TestResult {
    let client = AlistClient::new("https://alist.example.com/base")?;
    let headers = client.build_headers(&HashMap::new())?;

    assert_eq!(
        headers.get(ORIGIN).and_then(|v| v.to_str().ok()),
        Some("https://alist.example.com")
    );
    assert_eq!(
        headers.get(REFERER).and_then(|v| v.to_str().ok()),
        Some("https://alist.example.com/base")
    );
    Ok(())
}

#[test]
fn test_client_creation_rejects_query_and_fragment_in_host() {
    let Err(err) = AlistClient::new("https://alist.example.com/base?token=secret#frag") else {
        panic!("query and fragment must not be accepted in provider host");
    };
    assert!(
        err.to_string().contains("query or fragment"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_client_creation_rejects_userinfo_in_host() {
    let Err(err) = AlistClient::new("https://user:pass@alist.example.com") else {
        panic!("userinfo must not be accepted in provider host");
    };
    assert!(
        err.to_string().contains("userinfo"),
        "unexpected error: {err}"
    );
}

#[test]
fn test_alist_resp_deserialize_success() -> TestResult {
    let json = r#"{"code": 200, "message": "success", "data": {"token": "abc123"}}"#;
    let resp: crate::alist::types::AlistResp<crate::alist::types::LoginData> =
        serde_json::from_str(json)?;
    assert_eq!(resp.code, 200);
    assert_eq!(resp.message, "success");
    assert_eq!(
        resp.data
            .ok_or_else(|| missing("login response data should deserialize"))?
            .token,
        "abc123"
    );
    Ok(())
}

#[test]
fn test_fs_list_content_to_proto() {
    let content = crate::alist::types::HttpFsListContent {
        name: "video.mp4".to_string(),
        size: 1024,
        is_dir: false,
        modified: 1_700_000_000,
        sign: "abc".to_string(),
        thumb: String::new(),
        r#type: 2,
    };
    let proto: crate::transport_dto::alist::fs_list_resp::FsListContent = content.into();
    assert_eq!(proto.name, "video.mp4");
    assert_eq!(proto.size, 1024);
    assert!(!proto.is_dir);
}

#[test]
fn test_fs_list_resp_to_proto() {
    let resp = crate::alist::types::HttpFsListResp {
        content: vec![
            crate::alist::types::HttpFsListContent {
                name: "a.mp4".to_string(),
                size: 100,
                is_dir: false,
                modified: 0,
                sign: String::new(),
                thumb: String::new(),
                r#type: 0,
            },
            crate::alist::types::HttpFsListContent {
                name: "folder".to_string(),
                size: 0,
                is_dir: true,
                modified: 0,
                sign: String::new(),
                thumb: String::new(),
                r#type: 1,
            },
        ],
        total: 2,
        readme: "readme text".to_string(),
        write: true,
        provider: "local".to_string(),
    };
    let proto: crate::transport_dto::alist::FsListResp = resp.into();
    assert_eq!(proto.total, 2);
    assert_eq!(proto.content.len(), 2);
    assert_eq!(proto.readme, "readme text");
    assert!(proto.write);
}
