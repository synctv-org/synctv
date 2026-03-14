fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/descriptor.bin")
        .bytes(".")
        .compile_with_config(prost_config, &["proto/stream.proto"], &["proto"])?;

    Ok(())
}
