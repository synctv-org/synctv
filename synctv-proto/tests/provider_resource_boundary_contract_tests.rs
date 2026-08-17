const ALLOWED_PROVIDER_RESOURCE_STREAMS: &[&str] = &[
    "synctv.provider.emby.EmbyProviderService/GetThumbnail",
    "synctv.provider.fnos.FnosProviderService/GetThumbnail",
    "synctv.provider.nextcloud.NextcloudProviderService/GetPreview",
    "synctv.provider.qnap.QnapProviderService/GetThumbnail",
    "synctv.provider.seafile.SeafileProviderService/GetThumbnail",
    "synctv.provider.synology.SynologyProviderService/GetImage",
];

#[test]
fn provider_services_only_stream_pre_add_resource_previews() {
    let mut actual_streams = Vec::new();

    for service in synctv_proto::PROVIDERS_DESCRIPTOR_POOL.services() {
        for method in service.methods() {
            let method_path = format!("{}/{}", service.full_name(), method.name());
            if method.is_server_streaming() {
                actual_streams.push(method_path.clone());
            }

            let normalized = method.name().to_ascii_lowercase();
            assert!(
                !normalized.contains("subtitle")
                    && !normalized.contains("danmaku")
                    && !normalized.contains("hls")
                    && !normalized.contains("dash")
                    && !normalized.contains("flv")
                    && !normalized.ends_with("resource")
                    && !normalized.starts_with("watch"),
                "room playback resource method leaked into provider service: {method_path}"
            );
        }
    }

    actual_streams.sort();
    let mut expected_streams = ALLOWED_PROVIDER_RESOURCE_STREAMS.to_vec();
    expected_streams.sort_unstable();
    assert_eq!(actual_streams, expected_streams);
}

#[test]
fn emby_provider_exposes_only_browse_thumbnail_stream() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.emby.EmbyProviderService")
        .expect("Emby provider service descriptor");

    assert_eq!(provider.methods().count(), 6);
    assert!(provider
        .methods()
        .any(|method| method.name() == "GetThumbnail" && method.is_server_streaming()));
}
