fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let out_dir = std::env::var("OUT_DIR")?;
    let mut prost_config = prost_config();
    prost_config.protoc_executable(protoc);
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(std::path::Path::new(&out_dir).join("descriptor.bin"))
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir(&out_dir)
        .compile_with_config(
            prost_config,
            &["proto/management.proto"],
            &[".", "..", "../synctv-proto"],
        )?;

    Ok(())
}

fn prost_config() -> tonic_prost_build::Config {
    let mut config = tonic_prost_build::Config::new();
    config.extern_path(".synctv.admin", "::synctv_proto::admin");
    config.extern_path(".synctv.client", "::synctv_proto::client");
    config.extern_path(".synctv.common", "::synctv_proto::common");
    config.extern_path(".synctv.provider.alist", "::synctv_proto::providers::alist");
    config.extern_path(
        ".synctv.provider.bilibili",
        "::synctv_proto::providers::bilibili",
    );
    config.extern_path(
        ".synctv.provider.common",
        "::synctv_proto::providers::common",
    );
    config.extern_path(".synctv.provider.emby", "::synctv_proto::providers::emby");
    config.extern_path(".synctv.provider.rtmp", "::synctv_proto::providers::rtmp");
    config
}
