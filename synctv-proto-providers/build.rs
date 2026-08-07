fn main() -> Result<(), Box<dyn std::error::Error>> {
    std::env::set_current_dir(concat!(env!("CARGO_MANIFEST_DIR"), "/../synctv-proto"))?;
    synctv_proto_build::build_providers_crate()
}
