fn main() -> Result<(), Box<dyn std::error::Error>> {
    let protoc = protoc_bin_vendored::protoc_bin_path()?;
    let mut prost_config = tonic_prost_build::Config::new();
    prost_config.protoc_executable(protoc);

    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .file_descriptor_set_path("src/proto/descriptor.bin")
        .out_dir("src/proto")
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
