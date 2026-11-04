// Shared MIME mapping for embedded Web UI assets.
//
// Included by `build.rs` (to bake content types into the generated asset
// table) and exported from the crate (so the mapping is unit-testable and
// has exactly one definition).

/// Maps an asset path to its HTTP `Content-Type`.
///
/// `main.dart.mjs` (the WebAssembly module loader emitted by
/// `flutter build web --wasm`) must be served as `text/javascript`:
/// browsers refuse `import()` of modules with a non-JavaScript MIME type.
pub fn content_type(path: &str) -> &'static str {
    match std::path::Path::new(path)
        .extension()
        .and_then(std::ffi::OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "text/javascript; charset=utf-8",
        "json" | "map" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => "application/octet-stream",
    }
}

/// Whether an asset with this content type should be pre-compressed at
/// build time (brotli/gzip) for wire transfer.
pub fn compressible_content_type(content_type: &str) -> bool {
    content_type.starts_with("text/")
        || matches!(
            content_type.split(';').next().unwrap_or_default(),
            "application/json"
                | "application/javascript"
                | "application/wasm"
                | "application/xml"
                | "image/svg+xml"
        )
}
