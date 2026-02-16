fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile client and admin proto files to src/
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src")
        .compile_protos(&["proto/client.proto", "proto/admin.proto", "proto/oauth2.proto"], &["."])?;

    // Compile provider proto files to src/providers/
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        .file_descriptor_set_path("src/providers/descriptor.bin")
        .type_attribute(".", "#[derive(serde::Serialize, serde::Deserialize)]")
        .out_dir("src/providers")
        .compile_protos(
            &[
                "proto/providers/bilibili.proto",
                "proto/providers/alist.proto",
                "proto/providers/emby.proto",
            ],
            &["."],
        )?;

    Ok(())
}
