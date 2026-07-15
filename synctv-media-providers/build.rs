use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

fn proto_files_in(path: &str) -> io::Result<Vec<PathBuf>> {
    let mut files = fs::read_dir(path)?
        .map(|entry| entry.map(|entry| entry.path()))
        .collect::<io::Result<Vec<_>>>()?;
    files.retain(|path| {
        path.extension()
            .is_some_and(|extension| extension == "proto")
    });
    files.sort();
    Ok(files)
}

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
    prost_config.protoc_executable(&protoc);

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
                "proto/douyin.proto",
                "proto/emby.proto",
            ],
            &["proto"],
        )?;

    let mut acfun_files = proto_files_in("proto/acfun/im.basic")?;
    acfun_files.extend(proto_files_in("proto/acfun/zt.live.interactive")?);
    let mut acfun_config = tonic_prost_build::Config::new();
    acfun_config
        .protoc_executable(&protoc)
        .out_dir(&proto_out_dir)
        .compile_protos(
            &acfun_files,
            &[
                PathBuf::from("proto/acfun/im.basic"),
                PathBuf::from("proto/acfun/zt.live.interactive"),
            ],
        )?;

    println!("cargo:rerun-if-changed=proto/alist.proto");
    println!("cargo:rerun-if-changed=proto/bilibili.proto");
    println!("cargo:rerun-if-changed=proto/douyin.proto");
    println!("cargo:rerun-if-changed=proto/emby.proto");
    println!("cargo:rerun-if-changed=proto/acfun/im.basic");
    println!("cargo:rerun-if-changed=proto/acfun/zt.live.interactive");

    Ok(())
}
