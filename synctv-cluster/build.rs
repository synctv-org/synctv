use std::env;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

const GENERATED_FILES: &[&str] = &["synctv.cluster.rs"];

fn build_proto_out_dir() -> Result<PathBuf, Box<dyn std::error::Error>> {
    env::var_os("OUT_DIR")
        .map(|path| PathBuf::from(path).join("proto"))
        .ok_or_else(|| {
            Box::new(io::Error::new(
                io::ErrorKind::NotFound,
                "OUT_DIR is not set by Cargo",
            )) as Box<dyn std::error::Error>
        })
}

fn regen_proto_enabled() -> bool {
    env::var("SYNCTV_REGEN_PROTO")
        .is_ok_and(|value| matches!(value.as_str(), "1" | "true" | "TRUE" | "yes" | "on"))
}

fn sync_generated_files(source_root: &Path) -> Result<(), Box<dyn std::error::Error>> {
    let destination_root = Path::new("src/grpc/proto");
    fs::create_dir_all(destination_root)?;
    for file in GENERATED_FILES {
        fs::copy(source_root.join(file), destination_root.join(file))?;
    }
    Ok(())
}

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let proto_out_dir = build_proto_out_dir()?;
    fs::create_dir_all(&proto_out_dir)?;
    println!(
        "cargo:rustc-env=SYNCTV_CLUSTER_PROTO_OUT_DIR={}",
        proto_out_dir.display()
    );
    println!("cargo:rerun-if-env-changed=SYNCTV_REGEN_PROTO");
    println!("cargo:rerun-if-changed=proto/cluster.proto");

    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .out_dir(&proto_out_dir)
        .compile_with_config(prost_config, &["proto/cluster.proto"], &["proto"])?;

    if regen_proto_enabled() {
        sync_generated_files(&proto_out_dir)?;
    }

    Ok(())
}
