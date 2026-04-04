fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);
    prost_config.extern_path(".synctv.admin", "::synctv_proto::admin");
    prost_config.extern_path(".synctv.client", "::synctv_proto::client");
    prost_config.extern_path(".synctv.common", "::synctv_proto::common");

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src")
        .compile_with_config(
            prost_config,
            &["proto/management.proto"],
            &[".", "../synctv-proto"],
        )?;

    Ok(())
}
