#[path = "src/build_support.rs"]
mod build_support;

use build_support::{
    build_fingerprint, collect_files, ensure_disjoint_directories, hash_files, load_config,
    prepare_git_source, project_watch_directories, resolve_path, FlutterBuild, WebUiSource,
};
use flate2::write::GzEncoder;
use flate2::Compression;
use sha2::{Digest, Sha256};
use std::env;
use std::ffi::{OsStr, OsString};
use std::fmt::Write as _;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

const CONFIG_ENV: &str = "SYNCTV_WEB_CONFIG";
const LEGACY_DIST_ENV: &str = "SYNCTV_WEB_DIST";
const CACHE_ENV: &str = "SYNCTV_WEB_CACHE_DIR";
const OFFLINE_ENV: &str = "SYNCTV_WEB_OFFLINE";
const FORCE_ENV: &str = "SYNCTV_WEB_FORCE_REBUILD";
const EXPORT_ENV: &str = "SYNCTV_WEB_EXPORT_DIR";
const COMPRESSION_CACHE_VERSION: &str = "br-q9-w22-gzip-best-v1";

fn main() {
    if let Err(error) = build() {
        panic!("Web UI build failed: {error}");
    }
}

fn build() -> Result<(), String> {
    let out_dir = required_path("OUT_DIR")?;
    if env::var_os("CARGO_FEATURE_EMBED").is_none() {
        fs::write(
            out_dir.join("web_assets.rs"),
            "pub const WEB_UI_AVAILABLE: bool = false;\n\
             pub const BUILD_FINGERPRINT: &str = \"disabled\";\n\
             pub static ASSETS: &[Asset] = &[];\n",
        )
        .map_err(|error| format!("failed to write disabled Web UI manifest: {error}"))?;
        return Ok(());
    }
    for name in [
        CONFIG_ENV,
        LEGACY_DIST_ENV,
        CACHE_ENV,
        OFFLINE_ENV,
        FORCE_ENV,
        EXPORT_ENV,
    ] {
        println!("cargo:rerun-if-env-changed={name}");
    }

    let manifest_dir = required_path("CARGO_MANIFEST_DIR")?;
    let control_base = manifest_dir.parent().unwrap_or(&manifest_dir);
    for config_name in [build_support::DEFAULT_CONFIG, build_support::LOCAL_CONFIG] {
        println!(
            "cargo:rerun-if-changed={}",
            manifest_dir.join(config_name).display()
        );
    }
    let explicit_config = env::var_os(CONFIG_ENV)
        .map(PathBuf::from)
        .map(|path| absolute_control_path(control_base, path));
    let legacy_dist = env::var_os(LEGACY_DIST_ENV)
        .map(PathBuf::from)
        .map(|path| absolute_control_path(control_base, path));
    let loaded = load_config(
        &manifest_dir,
        explicit_config.as_deref(),
        legacy_dist.as_deref(),
    )?;
    println!("cargo:rerun-if-changed={}", loaded.path.display());

    let cache_dir = env::var_os(CACHE_ENV).map_or_else(
        || {
            manifest_dir
                .parent()
                .unwrap_or(&manifest_dir)
                .join("target/web-ui-cache")
        },
        |value| absolute_control_path(control_base, PathBuf::from(value)),
    );
    fs::create_dir_all(&cache_dir).map_err(|error| {
        format!(
            "failed to create Web UI cache {}: {error}",
            cache_dir.display()
        )
    })?;
    let offline = env_flag(OFFLINE_ENV)?;
    let force = env_flag(FORCE_ENV)?;

    let (dist, source_identity, flutter_version) = match &loaded.config.source {
        WebUiSource::Dist { path } => {
            let dist = resolve_path(&loaded.path, path);
            watch_tree(&dist, false)?;
            let files = collect_files(&dist, false)?;
            let identity = format!("dist:{}", hash_files(&dist, &files)?);
            (dist, identity, "prebuilt".to_owned())
        }
        WebUiSource::LocalProject { path, allow_dirty } => {
            let project = resolve_path(&loaded.path, path);
            if !allow_dirty {
                ensure_clean_checkout(&project)?;
            }
            let files = watch_tree(&project, true)?;
            let identity = format!("local:{}", hash_files(&project, &files)?);
            build_flutter_project(
                &project,
                &identity,
                &loaded.config.build,
                &cache_dir,
                offline,
                force,
            )?
        }
        WebUiSource::Git {
            repository,
            revision,
            commit,
        } => {
            let project =
                prepare_git_source(repository, revision, commit, &cache_dir, offline, force)?;
            let identity = format!("git:{repository}@{commit}");
            build_flutter_project(
                &project,
                &identity,
                &loaded.config.build,
                &cache_dir,
                offline,
                force,
            )?
        }
    };

    if !dist.join("index.html").is_file() {
        return Err(format!(
            "Web UI output {} does not contain index.html",
            dist.display()
        ));
    }
    if let Some(export_dir) = env::var_os(EXPORT_ENV)
        .map(PathBuf::from)
        .map(|path| absolute_control_path(control_base, path))
    {
        ensure_disjoint_directories(&dist, &export_dir)?;
        replace_directory(&dist, &export_dir)?;
    }
    let files = collect_files(&dist, false)?;
    let fingerprint = build_fingerprint(
        &format!("{source_identity}:{}", hash_files(&dist, &files)?),
        &flutter_version,
        &loaded.config.build,
    );
    generate_assets(&dist, &files, &out_dir, &cache_dir, &fingerprint)
}

fn replace_directory(source: &Path, destination: &Path) -> Result<(), String> {
    if destination.exists() {
        fs::remove_dir_all(destination).map_err(|error| {
            format!(
                "failed to clear Web UI export {}: {error}",
                destination.display()
            )
        })?;
    }
    copy_directory(source, destination)
}

fn copy_directory(source: &Path, destination: &Path) -> Result<(), String> {
    fs::create_dir_all(destination).map_err(|error| {
        format!(
            "failed to create Web UI export {}: {error}",
            destination.display()
        )
    })?;
    for entry in fs::read_dir(source)
        .map_err(|error| format!("failed to read {}: {error}", source.display()))?
    {
        let entry =
            entry.map_err(|error| format!("failed to inspect {}: {error}", source.display()))?;
        let destination_path = destination.join(entry.file_name());
        if entry.path().is_dir() {
            copy_directory(&entry.path(), &destination_path)?;
        } else if entry.path().is_file() {
            fs::copy(entry.path(), &destination_path).map_err(|error| {
                format!(
                    "failed to export Web asset {}: {error}",
                    entry.path().display()
                )
            })?;
        }
    }
    Ok(())
}

fn required_path(name: &str) -> Result<PathBuf, String> {
    env::var_os(name)
        .map(PathBuf::from)
        .ok_or_else(|| format!("{name} is not set"))
}

fn absolute_control_path(base: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        base.join(path)
    }
}

fn env_flag(name: &str) -> Result<bool, String> {
    let Some(value) = env::var_os(name) else {
        return Ok(false);
    };
    match value.to_string_lossy().trim().to_ascii_lowercase().as_str() {
        "" | "0" | "false" | "no" | "off" => Ok(false),
        "1" | "true" | "yes" | "on" => Ok(true),
        _ => Err(format!("{name} must be a boolean value")),
    }
}

fn watch_tree(root: &Path, project: bool) -> Result<Vec<PathBuf>, String> {
    if !root.is_dir() {
        return Err(format!(
            "Web UI source directory {} is missing",
            root.display()
        ));
    }
    if !project {
        println!("cargo:rerun-if-changed={}", root.display());
    }
    let files = collect_files(root, project)?;
    if project {
        for directory in project_watch_directories(root)? {
            println!("cargo:rerun-if-changed={}", directory.display());
        }
        let git_index = root.join(".git/index");
        if git_index.is_file() {
            println!("cargo:rerun-if-changed={}", git_index.display());
        }
    }
    for path in &files {
        println!("cargo:rerun-if-changed={}", path.display());
    }
    Ok(files)
}

fn ensure_clean_checkout(project: &Path) -> Result<(), String> {
    let output = run_output(
        Command::new("git").arg("-C").arg(project).args([
            "status",
            "--porcelain",
            "--untracked-files=normal",
        ]),
        "inspect local Web UI checkout",
    )?;
    if output.stdout.is_empty() {
        Ok(())
    } else {
        Err(format!(
            "local Web UI checkout {} has uncommitted changes; set source.allow-dirty=true for development",
            project.display()
        ))
    }
}

fn build_flutter_project(
    project: &Path,
    source_identity: &str,
    build: &FlutterBuild,
    cache_dir: &Path,
    offline: bool,
    force: bool,
) -> Result<(PathBuf, String, String), String> {
    if !project.join("pubspec.yaml").is_file() {
        return Err(format!(
            "Flutter Web project {} does not contain pubspec.yaml",
            project.display()
        ));
    }
    let flutter_version = command_stdout(
        Command::new(&build.flutter).args(["--version", "--machine"]),
        "read Flutter version",
    )?;
    let fingerprint = build_fingerprint(source_identity, &flutter_version, build);
    let output = cache_dir.join("builds").join(&fingerprint).join("web");
    if output.join("index.html").is_file() && !force {
        return Ok((output, source_identity.to_owned(), flutter_version));
    }

    let temporary = cache_dir.join("builds").join(".tmp").join(format!(
        "{}-{}",
        &fingerprint[..20],
        std::process::id()
    ));
    if temporary.exists() {
        fs::remove_dir_all(&temporary).map_err(|error| {
            format!(
                "failed to clear temporary Web UI output {}: {error}",
                temporary.display()
            )
        })?;
    }
    fs::create_dir_all(&temporary).map_err(|error| {
        format!(
            "failed to create temporary Web UI output {}: {error}",
            temporary.display()
        )
    })?;

    let mut pub_get = Command::new(&build.flutter);
    pub_get.current_dir(project).args(["pub", "get"]);
    if offline {
        pub_get.arg("--offline");
    }
    run_visible(&mut pub_get, "resolve Flutter Web dependencies")?;

    let temporary_dist = temporary.join("web");
    let mut flutter_build = Command::new(&build.flutter);
    flutter_build
        .current_dir(project)
        .args(["build", "web", "--no-pub", "--output"])
        .arg(&temporary_dist)
        .args(&build.arguments);
    for (key, value) in &build.dart_defines {
        flutter_build
            .arg("--dart-define")
            .arg(format!("{key}={value}"));
    }
    run_visible(&mut flutter_build, "build Flutter Web client")?;
    if !temporary_dist.join("index.html").is_file() {
        return Err("Flutter Web build completed without index.html".to_owned());
    }

    let parent = output
        .parent()
        .ok_or_else(|| "invalid Web UI build cache path".to_owned())?;
    fs::create_dir_all(parent).map_err(|error| {
        format!(
            "failed to create Web UI build cache {}: {error}",
            parent.display()
        )
    })?;
    if output.exists() {
        fs::remove_dir_all(&output).map_err(|error| {
            format!(
                "failed to replace Web UI cache {}: {error}",
                output.display()
            )
        })?;
    }
    fs::rename(&temporary_dist, &output).map_err(|error| {
        format!(
            "failed to publish Web UI cache {}: {error}",
            output.display()
        )
    })?;
    let _ = fs::remove_dir_all(&temporary);
    Ok((output, source_identity.to_owned(), flutter_version))
}

fn generate_assets(
    dist: &Path,
    files: &[PathBuf],
    out_dir: &Path,
    cache_dir: &Path,
    fingerprint: &str,
) -> Result<(), String> {
    let compression_cache = cache_dir.join("compressed");
    fs::create_dir_all(&compression_cache).map_err(|error| {
        format!(
            "failed to create compression cache {}: {error}",
            compression_cache.display()
        )
    })?;
    let mut source = format!(
        "pub const WEB_UI_AVAILABLE: bool = {};\npub const BUILD_FINGERPRINT: &str = {fingerprint:?};\npub static ASSETS: &[Asset] = &[\n",
        !files.is_empty()
    );
    for path in files {
        let relative = path
            .strip_prefix(dist)
            .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
        let route = relative
            .components()
            .map(|component| component.as_os_str().to_string_lossy())
            .collect::<Vec<_>>()
            .join("/");
        if route.is_empty() {
            continue;
        }
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read Web asset {}: {error}", path.display()))?;
        let content_type = content_type(&route);
        let etag = bytes_etag(&bytes);
        let path_literal = rust_string_literal(&path.to_string_lossy());
        let (brotli, gzip) = if compressible_content_type(content_type) {
            let digest = hex::encode(Sha256::digest(&bytes));
            let brotli_path =
                compression_cache.join(format!("{COMPRESSION_CACHE_VERSION}-{digest}.br"));
            let gzip_path =
                compression_cache.join(format!("{COMPRESSION_CACHE_VERSION}-{digest}.gz"));
            ensure_compressed(&bytes, &brotli_path, &gzip_path)?;
            (
                encoded_asset_source(&brotli_path)?,
                encoded_asset_source(&gzip_path)?,
            )
        } else {
            ("None".to_owned(), "None".to_owned())
        };
        writeln!(
            source,
            "    Asset {{ path: {route:?}, content_type: {content_type:?}, etag: {etag:?}, bytes: include_bytes!({path_literal}), brotli: {brotli}, gzip: {gzip} }},"
        )
        .map_err(|error| format!("failed to render Web asset table: {error}"))?;
    }
    source.push_str("];\n");
    fs::write(out_dir.join("web_assets.rs"), source)
        .map_err(|error| format!("failed to write generated Web asset table: {error}"))
}

fn ensure_compressed(bytes: &[u8], brotli_path: &Path, gzip_path: &Path) -> Result<(), String> {
    if !brotli_path.is_file() {
        let mut encoded = Vec::new();
        {
            let mut encoder = brotli::CompressorWriter::new(&mut encoded, 64 * 1024, 9, 22);
            encoder
                .write_all(bytes)
                .map_err(|error| format!("failed to Brotli-compress Web asset: {error}"))?;
        }
        fs::write(brotli_path, encoded).map_err(|error| {
            format!(
                "failed to write Brotli cache {}: {error}",
                brotli_path.display()
            )
        })?;
    }
    if !gzip_path.is_file() {
        let mut encoder = GzEncoder::new(Vec::new(), Compression::best());
        encoder
            .write_all(bytes)
            .map_err(|error| format!("failed to gzip Web asset: {error}"))?;
        let encoded = encoder
            .finish()
            .map_err(|error| format!("failed to finish gzip Web asset: {error}"))?;
        fs::write(gzip_path, encoded).map_err(|error| {
            format!(
                "failed to write gzip cache {}: {error}",
                gzip_path.display()
            )
        })?;
    }
    Ok(())
}

fn encoded_asset_source(path: &Path) -> Result<String, String> {
    let bytes = fs::read(path)
        .map_err(|error| format!("failed to read encoded asset {}: {error}", path.display()))?;
    let path_literal = rust_string_literal(&path.to_string_lossy());
    Ok(format!(
        "Some(EncodedAsset {{ etag: {:?}, bytes: include_bytes!({path_literal}) }})",
        bytes_etag(&bytes),
    ))
}

fn rust_string_literal(value: &str) -> String {
    let escaped = value
        .chars()
        .flat_map(char::escape_default)
        .collect::<String>();
    format!("\"{escaped}\"")
}

fn bytes_etag(bytes: &[u8]) -> String {
    format!("\"{}-{}\"", hex::encode(Sha256::digest(bytes)), bytes.len())
}

fn content_type(path: &str) -> &'static str {
    match Path::new(path)
        .extension()
        .and_then(OsStr::to_str)
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "html" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" => "text/javascript; charset=utf-8",
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

fn run_visible(command: &mut Command, description: &str) -> Result<(), String> {
    let status = command
        .status()
        .map_err(|error| format!("failed to {description}: {error}"))?;
    if status.success() {
        Ok(())
    } else {
        Err(format!("failed to {description} ({status})"))
    }
}

fn run_output(command: &mut Command, description: &str) -> Result<Output, String> {
    command
        .output()
        .map_err(|error| format!("failed to {description}: {error}"))
}

fn command_stdout(command: &mut Command, description: &str) -> Result<String, String> {
    let output = run_output(command, description)?;
    if !output.status.success() {
        return Err(command_failure(description, &output));
    }
    String::from_utf8(output.stdout)
        .map(|value| value.trim().to_owned())
        .map_err(|error| format!("{description} returned non-UTF-8 output: {error}"))
}

fn command_failure(description: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let details = if stderr.is_empty() { stdout } else { stderr };
    format!("failed to {description} ({}): {details}", output.status)
}

#[allow(dead_code)]
fn _display_command(program: &OsStr, arguments: &[OsString]) -> String {
    std::iter::once(program.to_string_lossy().into_owned())
        .chain(
            arguments
                .iter()
                .map(|argument| argument.to_string_lossy().into_owned()),
        )
        .collect::<Vec<_>>()
        .join(" ")
}
