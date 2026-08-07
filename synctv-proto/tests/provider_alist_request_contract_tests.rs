use synctv_proto::providers::alist::ListRequest;

#[test]
fn test_alist_list_request_uses_lower_camel_case_fields() {
    let request: ListRequest =
        serde_json::from_str(r#"{"serverId":"alist-main","path":"/","instanceName":"alist"}"#)
            .expect("lowerCamelCase Alist list request should deserialize");

    assert_eq!(request.server_id, "alist-main");
    assert_eq!(request.path, "/");
    assert_eq!(request.instance_name, "alist");

    let error = serde_json::from_str::<ListRequest>(
        r#"{"server_id":"alist-main","path":"/","instance_name":"alist"}"#,
    )
    .expect_err("snake_case Alist list request fields should be rejected");
    assert!(error.to_string().contains("server_id"));
}
