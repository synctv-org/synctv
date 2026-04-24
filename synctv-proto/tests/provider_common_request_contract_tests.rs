use synctv_proto::providers::common::{
    AddProviderInstanceRequest, ListProviderInstancesRequest, ProviderInstanceQuery,
    UpdateProviderInstanceRequest,
};

#[test]
fn test_provider_common_list_provider_instances_defaults_http_query_fields() {
    let request: ListProviderInstancesRequest =
        serde_json::from_str(r#"{"provider_type":"alist"}"#)
            .expect("provider list HTTP query should deserialize with default pagination");

    assert_eq!(request.page, 0);
    assert_eq!(request.page_size, 0);
    assert_eq!(request.provider_type, "alist");
    assert!(request.search.is_empty());
    assert_eq!(request.enabled, None);
    assert_eq!(request.tls, None);
    assert_eq!(request.sort_by, 0);
    assert_eq!(request.sort_direction, 0);
    synctv_proto::validate(&request).expect("defaulted provider list query should be valid");
}

#[test]
fn test_provider_common_list_provider_instances_rejects_too_long_search() {
    let request = ListProviderInstancesRequest {
        page: 1,
        page_size: 20,
        provider_type: String::new(),
        search: "a".repeat(101),
        enabled: None,
        tls: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("search"), "{message}");
}

#[test]
fn test_provider_common_list_provider_instances_rejects_invalid_provider_type_format() {
    let request = ListProviderInstancesRequest {
        page: 1,
        page_size: 20,
        provider_type: "Bad Provider".to_string(),
        search: String::new(),
        enabled: None,
        tls: None,
        sort_by: 0,
        sort_direction: 0,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("provider_type"), "{message}");
}

#[test]
fn test_provider_common_instance_query_rejects_invalid_name() {
    let request = ProviderInstanceQuery {
        instance_name: "../../../etc/passwd".to_string(),
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("instance_name"), "{message}");
}

#[test]
fn test_provider_common_add_provider_request_requires_valid_providers() {
    let request = AddProviderInstanceRequest {
        name: "alist_remote".to_string(),
        endpoint: "https://provider.example.com".to_string(),
        comment: String::new(),
        timeout_seconds: 10,
        tls: true,
        insecure_tls: false,
        providers: Vec::new(),
        jwt_secret: None,
        custom_ca: None,
    };

    let error = synctv_proto::validate(&request).expect_err("request should be invalid");
    let message = error.to_string();
    assert!(message.contains("providers"), "{message}");
}

#[test]
fn test_provider_common_update_provider_request_defaults_path_and_repeated_fields() {
    let request: UpdateProviderInstanceRequest =
        serde_json::from_str(r#"{"endpoint":"https://provider.example.com"}"#)
            .expect("provider common update request should deserialize");

    assert!(request.name.is_empty());
    assert_eq!(
        request.endpoint.as_deref(),
        Some("https://provider.example.com")
    );
    assert!(request.providers.is_empty());
    assert_eq!(request.jwt_secret, None);
    assert_eq!(request.custom_ca, None);
    assert_eq!(request.clear_comment, None);
    assert_eq!(request.clear_jwt_secret, None);
    assert_eq!(request.clear_custom_ca, None);
}
