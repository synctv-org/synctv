fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/cluster.proto");

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir("src/grpc/proto")
        .compile_with_config(prost_config, &["proto/cluster.proto"], &["proto"])?;

    Ok(())
}
