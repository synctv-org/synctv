fn main() -> Result<(), Box<dyn std::error::Error>> {
    // Compile stream.proto for internal stream relay service
    // This is service-to-service communication, not exposed externally
    tonic_prost_build::configure()
        .build_server(true)
        .build_client(true)
        // Use bytes::Bytes instead of Vec<u8> for proto `bytes` fields,
        // enabling zero-copy pass-through from FrameData (which also uses Bytes).
        .bytes(".")
        .compile_protos(&["proto/stream.proto"], &["proto"])?;

    Ok(())
}
