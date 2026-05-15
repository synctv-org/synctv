use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

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

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let proto_out_dir = build_proto_out_dir()?;
    fs::create_dir_all(&proto_out_dir)?;
    println!(
        "cargo:rustc-env=SYNCTV_MEDIA_PROVIDERS_PROTO_OUT_DIR={}",
        proto_out_dir.display()
    );

    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .file_descriptor_set_path(proto_out_dir.join("descriptor.bin"))
        .out_dir(&proto_out_dir)
        .compile_with_config(
            prost_config,
            &[
                "proto/alist.proto",
                "proto/bilibili.proto",
                "proto/emby.proto",
            ],
            &["proto"],
        )?;

    println!("cargo:rerun-if-changed=proto/alist.proto");
    println!("cargo:rerun-if-changed=proto/bilibili.proto");
    println!("cargo:rerun-if-changed=proto/emby.proto");

    Ok(())
}
