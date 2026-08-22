use std::env;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};

use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};

fn main() {
    println!("cargo:rerun-if-env-changed=SYNCTV_WEB_DIST");

    let out_dir = PathBuf::from(env::var_os("OUT_DIR").expect("OUT_DIR is set by Cargo"));
    let manifest_dir = PathBuf::from(env::var_os("CARGO_MANIFEST_DIR").expect("manifest dir"));
    let configured = env::var_os("SYNCTV_WEB_DIST").map(PathBuf::from);
    let dist = configured.unwrap_or_else(|| manifest_dir.join("web-dist"));
    let generated = out_dir.join("web_assets.rs");

    println!("cargo:rerun-if-changed={}", dist.display());

    let mut assets = Vec::new();
    if dist.is_dir() {
        collect_assets(&dist, &dist, &mut assets);
    }
    assets.sort_by(|left, right| left.0.cmp(&right.0));

    let mut source = String::from(
        "#[derive(Clone, Copy)]\n".to_owned()
            + "pub(crate) struct Asset {\n"
            + "    pub(crate) path: &'static str,\n"
            + "    pub(crate) content_type: &'static str,\n"
            + "    pub(crate) etag: &'static str,\n"
            + "    pub(crate) bytes: &'static [u8],\n"
            + "    pub(crate) brotli: Option<EncodedAsset>,\n"
            + "    pub(crate) gzip: Option<EncodedAsset>,\n"
            + "}\n\n"
            + "#[derive(Clone, Copy)]\n"
            + "pub(crate) struct EncodedAsset {\n"
            + "    pub(crate) etag: &'static str,\n"
            + "    pub(crate) bytes: &'static [u8],\n"
            + "}\n\n",
    );
    source.push_str(&format!(
        "pub(crate) const WEB_UI_AVAILABLE: bool = {};\n",
        !assets.is_empty()
    ));
    source.push_str("pub(crate) static ASSETS: &[Asset] = &[\n");
    for (index, (path, file)) in assets.into_iter().enumerate() {
        let content_type = content_type(&path);
        let etag = asset_etag(&file);
        let encoded = if compressible_content_type(content_type) {
            Some(precompress_asset(&file, &out_dir, index))
        } else {
            None
        };
        let (brotli, gzip) = encoded.map_or_else(
            || ("None".to_owned(), "None".to_owned()),
            |(brotli_file, gzip_file)| {
                (
                    encoded_asset_source(&brotli_file),
                    encoded_asset_source(&gzip_file),
                )
            },
        );
        source.push_str(&format!(
            "    Asset {{ path: {path:?}, content_type: {content_type:?}, etag: {etag:?}, bytes: include_bytes!({file:?}), brotli: {brotli}, gzip: {gzip} }},\n"
        ));
    }
    source.push_str("];\n");
    fs::write(generated, source).expect("write generated web asset table");
}

fn collect_assets(root: &Path, directory: &Path, assets: &mut Vec<(String, PathBuf)>) {
    let entries = match fs::read_dir(directory) {
        Ok(entries) => entries,
        Err(error) => {
            println!(
                "cargo:warning=Unable to read Web asset directory {}: {error}",
                directory.display()
            );
            return;
        }
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_assets(root, &path, assets);
            continue;
        }
        if !path.is_file() {
            continue;
        }
        let relative = match path.strip_prefix(root) {
            Ok(relative) => relative,
            Err(_) => continue,
        };
        let route = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if route.is_empty() {
            continue;
        }
        println!("cargo:rerun-if-changed={}", path.display());
        assets.push((route, path));
    }
}

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(|extension| extension.to_str())
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "svg" => "image/svg+xml",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "webp" => "image/webp",
        "ico" => "image/x-icon",
        "wasm" => "application/wasm",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        "map" => "application/json; charset=utf-8",
        _ => "application/octet-stream",
    }
}

fn asset_etag(path: &Path) -> String {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("failed to hash Web asset {}: {error}", path.display()));
    let digest = Sha256::digest(&bytes);
    format!("\"{}-{}\"", hex::encode(digest), bytes.len())
}

fn compressible_content_type(content_type: &str) -> bool {
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

fn precompress_asset(path: &Path, out_dir: &Path, index: usize) -> (PathBuf, PathBuf) {
    let bytes = fs::read(path)
        .unwrap_or_else(|error| panic!("failed to compress Web asset {}: {error}", path.display()));

    let brotli_file = out_dir.join(format!("web_asset_{index}.br"));
    let mut brotli_bytes = Vec::new();
    {
        let mut encoder = brotli::CompressorWriter::new(&mut brotli_bytes, 64 * 1024, 9, 22);
        encoder.write_all(&bytes).unwrap_or_else(|error| {
            panic!(
                "failed to Brotli-compress Web asset {}: {error}",
                path.display()
            )
        });
    }
    fs::write(&brotli_file, brotli_bytes).unwrap_or_else(|error| {
        panic!(
            "failed to write compressed Web asset {}: {error}",
            brotli_file.display()
        )
    });

    let gzip_file = out_dir.join(format!("web_asset_{index}.gz"));
    let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
    encoder
        .write_all(&bytes)
        .unwrap_or_else(|error| panic!("failed to gzip Web asset {}: {error}", path.display()));
    let gzip_bytes = encoder.finish().unwrap_or_else(|error| {
        panic!(
            "failed to finish gzip Web asset {}: {error}",
            path.display()
        )
    });
    fs::write(&gzip_file, gzip_bytes).unwrap_or_else(|error| {
        panic!(
            "failed to write compressed Web asset {}: {error}",
            gzip_file.display()
        )
    });

    (brotli_file, gzip_file)
}

fn encoded_asset_source(path: &Path) -> String {
    let etag = asset_etag(path);
    format!("Some(EncodedAsset {{ etag: {etag:?}, bytes: include_bytes!({path:?}) }})")
}
