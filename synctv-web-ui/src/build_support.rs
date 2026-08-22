use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

pub const BUILDER_VERSION: &str = "synctv-web-ui-v3";
pub const DEFAULT_CONFIG: &str = "web-ui.toml";
pub const LOCAL_CONFIG: &str = "web-ui.local.toml";

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(rename_all = "kebab-case", deny_unknown_fields)]
pub struct WebUiConfig {
    pub schema_version: u32,
    pub source: WebUiSource,
    #[serde(default)]
    pub build: FlutterBuild,
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(
    tag = "kind",
    rename_all = "kebab-case",
    rename_all_fields = "kebab-case",
    deny_unknown_fields
)]
pub enum WebUiSource {
    Dist {
        path: PathBuf,
    },
    LocalProject {
        path: PathBuf,
        #[serde(default = "default_allow_dirty")]
        allow_dirty: bool,
    },
    Git {
        repository: String,
        revision: String,
        commit: String,
    },
}

const fn default_allow_dirty() -> bool {
    true
}

#[derive(Clone, Debug, Deserialize, Eq, PartialEq)]
#[serde(default, rename_all = "kebab-case", deny_unknown_fields)]
pub struct FlutterBuild {
    pub flutter: String,
    pub arguments: Vec<String>,
    pub dart_defines: BTreeMap<String, String>,
}

impl Default for FlutterBuild {
    fn default() -> Self {
        Self {
            flutter: "flutter".to_owned(),
            arguments: vec![
                "--release".to_owned(),
                "--no-web-resources-cdn".to_owned(),
                "--no-wasm-dry-run".to_owned(),
            ],
            dart_defines: BTreeMap::new(),
        }
    }
}

#[derive(Debug)]
pub struct LoadedConfig {
    pub config: WebUiConfig,
    pub path: PathBuf,
}

pub fn load_config(
    manifest_dir: &Path,
    explicit: Option<&Path>,
    legacy_dist: Option<&Path>,
) -> Result<LoadedConfig, String> {
    if let Some(path) = legacy_dist {
        return Ok(LoadedConfig {
            config: WebUiConfig {
                schema_version: 1,
                source: WebUiSource::Dist {
                    path: path.to_path_buf(),
                },
                build: FlutterBuild::default(),
            },
            path: manifest_dir.join(LOCAL_CONFIG),
        });
    }
    let path = explicit.map_or_else(
        || {
            let local = manifest_dir.join(LOCAL_CONFIG);
            if local.is_file() {
                local
            } else {
                manifest_dir.join(DEFAULT_CONFIG)
            }
        },
        Path::to_path_buf,
    );
    let text = fs::read_to_string(&path)
        .map_err(|error| format!("failed to read Web UI config {}: {error}", path.display()))?;
    let config: WebUiConfig = toml::from_str(&text)
        .map_err(|error| format!("invalid Web UI config {}: {error}", path.display()))?;
    if config.schema_version != 1 {
        return Err(format!(
            "unsupported Web UI config schema {} in {}",
            config.schema_version,
            path.display()
        ));
    }
    validate_config(&config)?;
    Ok(LoadedConfig { config, path })
}

fn validate_config(config: &WebUiConfig) -> Result<(), String> {
    if config.build.flutter.trim().is_empty() {
        return Err("build.flutter must not be empty".to_owned());
    }
    if config
        .build
        .arguments
        .iter()
        .any(|argument| argument.contains('\n') || argument.contains('\r'))
    {
        return Err("build.arguments must not contain line breaks".to_owned());
    }
    if let WebUiSource::Git {
        repository,
        revision,
        commit,
    } = &config.source
    {
        if repository.trim().is_empty() || revision.trim().is_empty() {
            return Err("Git repository and revision must not be empty".to_owned());
        }
        if repository.contains(['\n', '\r']) || revision.contains(['\n', '\r']) {
            return Err("Git source values must not contain line breaks".to_owned());
        }
        if repository
            .split_once("://")
            .and_then(|(_, remainder)| remainder.split('/').next())
            .is_some_and(|authority| authority.contains('@'))
        {
            return Err("Git repository URLs must not contain credentials".to_owned());
        }
        if !is_full_commit(commit) {
            return Err("Git commit must be a full lowercase SHA-1".to_owned());
        }
    }
    Ok(())
}

fn is_full_commit(value: &str) -> bool {
    value.len() == 40
        && value
            .as_bytes()
            .iter()
            .all(|byte| byte.is_ascii_digit() || matches!(byte, b'a'..=b'f'))
}

pub fn resolve_path(config_path: &Path, path: &Path) -> PathBuf {
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        config_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}

pub fn collect_files(root: &Path, project: bool) -> Result<Vec<PathBuf>, String> {
    if project {
        if let Some(files) = git_project_files(root)? {
            return Ok(files);
        }
    }
    let mut files = Vec::new();
    collect_files_inner(root, root, project, &mut files)?;
    files.sort();
    Ok(files)
}

pub fn project_watch_directories(root: &Path) -> Result<Vec<PathBuf>, String> {
    let mut directories = BTreeSet::new();
    collect_project_directories(root, root, &mut directories)?;
    directories.remove(root);
    Ok(directories.into_iter().collect())
}

fn collect_project_directories(
    root: &Path,
    directory: &Path,
    directories: &mut BTreeSet<PathBuf>,
) -> Result<(), String> {
    directories.insert(directory.to_path_buf());
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to inspect an entry in {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if path.is_dir() && !excluded_project_directory(&path) && path.starts_with(root) {
            collect_project_directories(root, &path, directories)?;
        }
    }
    Ok(())
}

pub fn ensure_disjoint_directories(source: &Path, destination: &Path) -> Result<(), String> {
    let source = source.canonicalize().map_err(|error| {
        format!(
            "failed to resolve Web UI source directory {}: {error}",
            source.display()
        )
    })?;
    let destination = resolve_existing_ancestor(destination)?;
    if source == destination || source.starts_with(&destination) || destination.starts_with(&source)
    {
        return Err(format!(
            "Web UI export {} must not equal, contain, or be contained by source {}",
            destination.display(),
            source.display()
        ));
    }
    Ok(())
}

pub fn prepare_git_source(
    repository: &str,
    revision: &str,
    commit: &str,
    cache_dir: &Path,
    offline: bool,
    force: bool,
) -> Result<PathBuf, String> {
    let repository_key = hex::encode(Sha256::digest(repository.as_bytes()));
    let project = cache_dir
        .join("git")
        .join(&repository_key[..24])
        .join(commit);
    let git_dir = project.join(".git");
    if !git_dir.is_dir() {
        if offline {
            return Err(format!(
                "offline Web UI build requires cached Git commit {commit}"
            ));
        }
        fs::create_dir_all(&project).map_err(|error| {
            format!("failed to create Git cache {}: {error}", project.display())
        })?;
        run(
            Command::new("git").arg("init").arg(&project),
            "initialize Web UI Git cache",
        )?;
        run(
            Command::new("git")
                .arg("-C")
                .arg(&project)
                .args(["remote", "add", "origin", repository]),
            "configure Web UI Git remote",
        )?;
    }

    let has_commit = command_succeeds(Command::new("git").arg("-C").arg(&project).args([
        "cat-file",
        "-e",
        &format!("{commit}^{{commit}}"),
    ]));
    let source_marker = project.join(".synctv-source");
    let expected_marker =
        format!("repository={repository}\nrevision={revision}\ncommit={commit}\n");
    let source_is_validated =
        fs::read_to_string(&source_marker).is_ok_and(|value| value == expected_marker);
    if !has_commit || !source_is_validated || force {
        if offline {
            return Err(format!(
                "offline Web UI build cannot validate uncached Git source {repository}@{revision} ({commit})"
            ));
        }
        run(
            Command::new("git").arg("-C").arg(&project).args([
                "fetch",
                "--depth=1",
                "--no-tags",
                "origin",
                revision,
            ]),
            "fetch Web UI Git revision",
        )?;
        let resolved = command_stdout(
            Command::new("git")
                .arg("-C")
                .arg(&project)
                .args(["rev-parse", "FETCH_HEAD^{commit}"]),
            "resolve Web UI Git revision",
        )?;
        if resolved != commit {
            return Err(format!(
                "Web UI revision {revision} resolved to {resolved}, expected pinned commit {commit}"
            ));
        }
        fs::write(&source_marker, expected_marker).map_err(|error| {
            format!(
                "failed to record validated Web UI Git source {}: {error}",
                source_marker.display()
            )
        })?;
    }
    run(
        Command::new("git")
            .arg("-C")
            .arg(&project)
            .args(["checkout", "--force", "--detach", commit]),
        "check out Web UI Git commit",
    )?;
    Ok(project)
}

fn run(command: &mut Command, description: &str) -> Result<(), String> {
    let output = run_output(command, description)?;
    if output.status.success() {
        Ok(())
    } else {
        Err(command_failure(description, &output))
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

fn command_succeeds(command: &mut Command) -> bool {
    command.output().is_ok_and(|output| output.status.success())
}

fn command_failure(description: &str, output: &Output) -> String {
    let stderr = String::from_utf8_lossy(&output.stderr).trim().to_owned();
    let stdout = String::from_utf8_lossy(&output.stdout).trim().to_owned();
    let details = if stderr.is_empty() { stdout } else { stderr };
    format!("failed to {description} ({}): {details}", output.status)
}

fn resolve_existing_ancestor(path: &Path) -> Result<PathBuf, String> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("failed to resolve current directory: {error}"))?
            .join(path)
    };
    let absolute = normalize_path(&absolute);
    let mut existing = absolute.as_path();
    let mut missing = Vec::new();
    while !existing.exists() {
        let name = existing.file_name().ok_or_else(|| {
            format!(
                "Web UI export path {} has no existing ancestor",
                path.display()
            )
        })?;
        missing.push(name.to_os_string());
        existing = existing.parent().ok_or_else(|| {
            format!(
                "Web UI export path {} has no existing ancestor",
                path.display()
            )
        })?;
    }
    let mut resolved = existing.canonicalize().map_err(|error| {
        format!(
            "failed to resolve Web UI export ancestor {}: {error}",
            existing.display()
        )
    })?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    Ok(resolved)
}

fn normalize_path(path: &Path) -> PathBuf {
    use std::path::Component;

    let mut normalized = PathBuf::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir => {
                normalized.pop();
            }
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                normalized.push(component.as_os_str());
            }
        }
    }
    normalized
}

fn git_project_files(root: &Path) -> Result<Option<Vec<PathBuf>>, String> {
    let output = match Command::new("git")
        .arg("-C")
        .arg(root)
        .args([
            "ls-files",
            "--cached",
            "--others",
            "--exclude-standard",
            "-z",
        ])
        .output()
    {
        Ok(output) if output.status.success() => output.stdout,
        Ok(_) => return Ok(None),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => {
            return Err(format!(
                "failed to enumerate Git files in {}: {error}",
                root.display()
            ));
        }
    };
    let mut files = Vec::new();
    for encoded in output
        .split(|byte| *byte == 0)
        .filter(|path| !path.is_empty())
    {
        let relative = std::str::from_utf8(encoded).map_err(|error| {
            format!(
                "Git returned a non-UTF-8 Web UI path in {}: {error}",
                root.display()
            )
        })?;
        let path = root.join(relative);
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    files.dedup();
    Ok(Some(files))
}

fn collect_files_inner(
    root: &Path,
    directory: &Path,
    project: bool,
    files: &mut Vec<PathBuf>,
) -> Result<(), String> {
    let entries = fs::read_dir(directory)
        .map_err(|error| format!("failed to read {}: {error}", directory.display()))?;
    for entry in entries {
        let entry = entry.map_err(|error| {
            format!(
                "failed to inspect an entry in {}: {error}",
                directory.display()
            )
        })?;
        let path = entry.path();
        if path.is_dir() {
            if project && excluded_project_directory(&path) {
                continue;
            }
            collect_files_inner(root, &path, project, files)?;
        } else if path.is_file()
            && path.strip_prefix(root).is_ok()
            && (!project || !excluded_project_file(&path))
        {
            files.push(path);
        }
    }
    Ok(())
}

fn excluded_project_directory(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some(".git" | ".dart_tool" | ".idea" | ".vscode" | "build" | "target")
    )
}

fn excluded_project_file(path: &Path) -> bool {
    matches!(
        path.file_name().and_then(OsStr::to_str),
        Some(".DS_Store" | ".flutter-plugins" | ".flutter-plugins-dependencies" | ".packages")
    )
}

pub fn hash_files(root: &Path, files: &[PathBuf]) -> Result<String, String> {
    let mut hasher = Sha256::new();
    for path in files {
        let relative = path
            .strip_prefix(root)
            .map_err(|error| format!("failed to relativize {}: {error}", path.display()))?;
        hasher.update(relative.to_string_lossy().as_bytes());
        hasher.update([0]);
        let bytes = fs::read(path)
            .map_err(|error| format!("failed to read {}: {error}", path.display()))?;
        hasher.update(bytes.len().to_le_bytes());
        hasher.update(bytes);
    }
    Ok(hex::encode(hasher.finalize()))
}

pub fn build_fingerprint(
    source_identity: &str,
    flutter_version: &str,
    build: &FlutterBuild,
) -> String {
    let mut hasher = Sha256::new();
    hasher.update(BUILDER_VERSION.as_bytes());
    hasher.update([0]);
    hasher.update(source_identity.as_bytes());
    hasher.update([0]);
    hasher.update(flutter_version.as_bytes());
    for argument in &build.arguments {
        hasher.update([0]);
        hasher.update(argument.as_bytes());
    }
    for (key, value) in &build.dart_defines {
        hasher.update([0]);
        hasher.update(key.as_bytes());
        hasher.update(b"=");
        hasher.update(value.as_bytes());
    }
    hex::encode(hasher.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn local_config_takes_precedence() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::write(
            directory.path().join(DEFAULT_CONFIG),
            "schema-version=1\n[source]\nkind='dist'\npath='default'\n",
        )?;
        fs::write(
            directory.path().join(LOCAL_CONFIG),
            "schema-version=1\n[source]\nkind='dist'\npath='local'\n",
        )?;

        let loaded = load_config(directory.path(), None, None)?;

        assert_eq!(loaded.path, directory.path().join(LOCAL_CONFIG));
        assert!(matches!(
            &loaded.config.source,
            WebUiSource::Dist { path }
                if resolve_path(&loaded.path, path) == directory.path().join("local")
        ));
        Ok(())
    }

    #[test]
    fn fingerprint_is_stable_for_ordered_defines() {
        let mut first = FlutterBuild::default();
        first.dart_defines.insert("B".to_owned(), "2".to_owned());
        first.dart_defines.insert("A".to_owned(), "1".to_owned());
        let mut second = FlutterBuild::default();
        second.dart_defines.insert("A".to_owned(), "1".to_owned());
        second.dart_defines.insert("B".to_owned(), "2".to_owned());

        assert_eq!(
            build_fingerprint("source", "flutter", &first),
            build_fingerprint("source", "flutter", &second)
        );
    }

    #[test]
    fn project_hash_ignores_generated_build_output() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join("lib"))?;
        fs::create_dir(directory.path().join("build"))?;
        fs::write(directory.path().join("lib/main.dart"), "void main() {}")?;
        fs::write(directory.path().join("build/output.js"), "generated")?;
        fs::write(
            directory.path().join(".flutter-plugins-dependencies"),
            "generated",
        )?;

        let files = collect_files(directory.path(), true)?;
        let initial_hash = hash_files(directory.path(), &files)?;
        fs::write(directory.path().join("build/output.js"), "changed")?;
        fs::write(
            directory.path().join(".flutter-plugins-dependencies"),
            "changed",
        )?;
        let next_files = collect_files(directory.path(), true)?;

        assert_eq!(files, vec![directory.path().join("lib/main.dart")]);
        assert_eq!(initial_hash, hash_files(directory.path(), &next_files)?);
        Ok(())
    }

    #[test]
    fn git_project_files_exclude_ignored_generated_inputs() -> Result<(), Box<dyn std::error::Error>>
    {
        let directory = tempfile::tempdir()?;
        fs::create_dir(directory.path().join("lib"))?;
        fs::create_dir(directory.path().join("generated"))?;
        fs::write(directory.path().join("lib/main.dart"), "void main() {}")?;
        fs::write(directory.path().join("lib/pending.dart"), "pending")?;
        fs::write(directory.path().join("generated/output"), "generated")?;
        fs::write(directory.path().join(".gitignore"), "generated/\n")?;
        assert!(Command::new("git")
            .arg("init")
            .arg(directory.path())
            .status()?
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(directory.path())
            .args(["add", ".gitignore", "lib/main.dart"])
            .status()?
            .success());

        let files = collect_files(directory.path(), true)?;
        let relative = files
            .iter()
            .map(|path| path.strip_prefix(directory.path()))
            .collect::<Result<Vec<_>, _>>()?;

        assert_eq!(
            relative,
            vec![
                Path::new(".gitignore"),
                Path::new("lib/main.dart"),
                Path::new("lib/pending.dart")
            ]
        );
        Ok(())
    }

    #[test]
    fn git_source_rejects_credentials_and_partial_commits() {
        let config = WebUiConfig {
            schema_version: 1,
            source: WebUiSource::Git {
                repository: "https://user:secret@example.com/repo".to_owned(),
                revision: "main".to_owned(),
                commit: "abc".to_owned(),
            },
            build: FlutterBuild::default(),
        };

        assert!(validate_config(&config).is_err());
    }

    #[test]
    fn project_watch_directories_cover_new_files_without_generated_output(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let root = directory.path();
        fs::create_dir_all(root.join("lib/empty"))?;
        fs::create_dir_all(root.join("packages/player/lib"))?;
        fs::create_dir_all(root.join("build/web"))?;

        assert_eq!(
            project_watch_directories(root)?,
            vec![
                root.join("lib"),
                root.join("lib/empty"),
                root.join("packages"),
                root.join("packages/player"),
                root.join("packages/player/lib")
            ]
        );
        Ok(())
    }

    #[test]
    fn export_must_be_disjoint_from_source() -> Result<(), Box<dyn std::error::Error>> {
        let directory = tempfile::tempdir()?;
        let source = directory.path().join("source");
        let sibling = directory.path().join("export");
        fs::create_dir_all(source.join("nested"))?;

        assert!(ensure_disjoint_directories(&source, &source).is_err());
        assert!(ensure_disjoint_directories(&source, &source.join("export")).is_err());
        assert!(ensure_disjoint_directories(&source.join("nested"), &source).is_err());
        ensure_disjoint_directories(&source, &sibling)?;
        Ok(())
    }

    #[test]
    fn offline_git_source_requires_cached_commit() -> Result<(), Box<dyn std::error::Error>> {
        let cache = tempfile::tempdir()?;
        let error = prepare_git_source(
            "https://example.invalid/app.git",
            "main",
            "a234567890abcdef0123456789abcdef01234567",
            cache.path(),
            true,
            false,
        )
        .expect_err("uncached offline source must fail");

        assert!(error.contains("requires cached Git commit"));
        Ok(())
    }

    #[test]
    fn git_source_rejects_revision_at_another_commit() -> Result<(), Box<dyn std::error::Error>> {
        let repository = create_test_repository()?;
        let revision = git_stdout(repository.path(), &["rev-parse", "HEAD"])?;
        let cache = tempfile::tempdir()?;
        let expected = if revision.starts_with('a') {
            "b".repeat(40)
        } else {
            "a".repeat(40)
        };

        let error = prepare_git_source(
            &repository.path().to_string_lossy(),
            &revision,
            &expected,
            cache.path(),
            false,
            false,
        )
        .expect_err("mismatched revision must fail");

        assert!(error.contains("resolved to"));
        assert!(error.contains("expected pinned commit"));
        Ok(())
    }

    #[test]
    fn cached_git_source_revalidates_a_changed_revision() -> Result<(), Box<dyn std::error::Error>>
    {
        let repository = create_test_repository()?;
        let first = git_stdout(repository.path(), &["rev-parse", "HEAD"])?;
        let cache = tempfile::tempdir()?;
        prepare_git_source(
            &repository.path().to_string_lossy(),
            &first,
            &first,
            cache.path(),
            false,
            false,
        )?;

        fs::write(repository.path().join("pubspec.yaml"), "name: changed\n")?;
        assert!(Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["add", "pubspec.yaml"])
            .status()?
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args([
                "-c",
                "user.name=SyncTV Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "changed fixture",
            ])
            .status()?
            .success());
        let second = git_stdout(repository.path(), &["rev-parse", "HEAD"])?;

        let error = prepare_git_source(
            &repository.path().to_string_lossy(),
            &second,
            &first,
            cache.path(),
            false,
            false,
        )
        .expect_err("a changed revision must be resolved again");

        assert!(error.contains("resolved to"));
        assert!(error.contains("expected pinned commit"));
        Ok(())
    }

    fn create_test_repository() -> Result<tempfile::TempDir, Box<dyn std::error::Error>> {
        let repository = tempfile::tempdir()?;
        assert!(Command::new("git")
            .arg("init")
            .arg(repository.path())
            .status()?
            .success());
        fs::write(repository.path().join("pubspec.yaml"), "name: app\n")?;
        assert!(Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args(["add", "pubspec.yaml"])
            .status()?
            .success());
        assert!(Command::new("git")
            .arg("-C")
            .arg(repository.path())
            .args([
                "-c",
                "user.name=SyncTV Test",
                "-c",
                "user.email=test@example.invalid",
                "commit",
                "-m",
                "fixture",
            ])
            .status()?
            .success());
        Ok(repository)
    }

    fn git_stdout(
        repository: &Path,
        arguments: &[&str],
    ) -> Result<String, Box<dyn std::error::Error>> {
        let output = Command::new("git")
            .arg("-C")
            .arg(repository)
            .args(arguments)
            .output()?;
        assert!(output.status.success());
        Ok(String::from_utf8(output.stdout)?.trim().to_owned())
    }
}
