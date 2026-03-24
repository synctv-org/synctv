fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc.clone());
    for field in [
        ".synctv.client.GetRoomRequest.room_id",
        ".synctv.client.JoinRoomRequest.room_id",
        ".synctv.client.LeaveRoomRequest.room_id",
        ".synctv.client.DeleteRoomRequest.room_id",
        ".synctv.client.CheckRoomPasswordRequest.room_id",
    ] {
        prost_config.field_attribute(field, "#[serde(default)]");
    }

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src")
        .compile_with_config(
            prost_config,
            &[
                "proto/client.proto",
                "proto/admin.proto",
                "proto/oauth2.proto",
            ],
            &["."],
        )?;

    let mut provider_prost_config = tonic_prost_build::Config::new();
    provider_prost_config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/providers/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src/providers")
        .compile_with_config(
            provider_prost_config,
            &[
                "proto/providers/bilibili.proto",
                "proto/providers/alist.proto",
                "proto/providers/emby.proto",
            ],
            &["."],
        )?;

    Ok(())
}
