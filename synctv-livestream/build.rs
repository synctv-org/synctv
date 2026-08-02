fn main() -> Result<(), Box<dyn std::error::Error>> {
    let out_dir = std::env::var_os("OUT_DIR")
        .map(std::path::PathBuf::from)
        .ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::NotFound, "OUT_DIR is not set by Cargo")
        })?;

    println!(
        "cargo:rustc-env=SYNCTV_LIVESTREAM_PROTO_OUT_DIR={}",
        out_dir.display()
    );
    println!("cargo:rerun-if-changed=proto/stream.proto");

    let prost_config = tonic_prost_build::Config::new();

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path(out_dir.join("descriptor.bin"))
        .out_dir(&out_dir)
        .bytes(".")
        .compile_with_config(prost_config, &["proto/stream.proto"], &["proto"])?;

    Ok(())
}
