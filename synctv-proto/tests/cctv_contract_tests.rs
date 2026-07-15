use prost_reflect::Kind;

#[test]
fn cctv_services_and_wire_contract_are_registered() {
    let provider = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_service_by_name("synctv.provider.cctv.CctvProviderService")
        .expect("CCTV provider service descriptor");
    assert_eq!(provider.methods().count(), 1);

    let playback = synctv_proto::PLAYBACK_PROVIDER_DESCRIPTOR_POOL
        .get_service_by_name("synctv.playback_provider.cctv.CctvPlaybackProviderService")
        .expect("CCTV playback provider service descriptor");
    assert_eq!(playback.methods().count(), 2);

    let chapter = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.cctv.Chapter")
        .expect("CCTV chapter descriptor");
    assert_eq!(
        chapter
            .get_field_by_name("start_ms")
            .expect("CCTV chapter start")
            .kind(),
        Kind::Uint64
    );
    assert_eq!(
        chapter
            .get_field_by_name("end_ms")
            .expect("CCTV chapter end")
            .kind(),
        Kind::Uint64
    );

    let metadata = synctv_proto::PROVIDERS_DESCRIPTOR_POOL
        .get_message_by_name("synctv.provider.cctv.Metadata")
        .expect("CCTV metadata descriptor");
    assert_eq!(
        metadata
            .get_field_by_name("published_at")
            .expect("CCTV published timestamp")
            .kind(),
        Kind::Int64
    );
}
