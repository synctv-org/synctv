use std::fs;
use std::path::{Path, PathBuf};

fn rust_files_under(path: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    let mut stack = vec![path.to_path_buf()];

    while let Some(current) = stack.pop() {
        let entries = fs::read_dir(&current)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", current.display()));

        for entry in entries {
            let entry =
                entry.unwrap_or_else(|error| panic!("failed to read directory entry: {error}"));
            let path = entry.path();
            if path.is_dir() {
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                files.push(path);
            }
        }
    }

    files
}

#[test]
fn provider_upstream_transport_uses_media_provider_transport_dto_boundary() {
    let manifest_dir = Path::new(env!("CARGO_MANIFEST_DIR"));
    let provider_dir = manifest_dir.join("src/provider");
    let forbidden = "synctv_media_providers::grpc";

    for path in rust_files_under(&provider_dir) {
        let content = fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("failed to read {}: {error}", path.display()));
        assert!(
            !content.contains(forbidden),
            "{} imports provider gRPC generated modules directly; use synctv_media_providers::transport_dto as the provider upstream boundary",
            path.display()
        );
    }
}
