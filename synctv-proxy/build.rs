fn main() -> Result<(), Box<dyn std::error::Error>> {
    println!("cargo:rerun-if-changed=proto/slice_cache.proto");

    let prost_config = tonic_prost_build::Config::new();

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .compile_with_config(prost_config, &["proto/slice_cache.proto"], &["proto"])?;

    Ok(())
}
