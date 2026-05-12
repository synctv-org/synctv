fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    std::env::set_var("PROTOC", protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_protos(&["proto/realtime.proto"], &["proto"])?;

    println!("cargo:rerun-if-changed=proto/realtime.proto");
    Ok(())
}
