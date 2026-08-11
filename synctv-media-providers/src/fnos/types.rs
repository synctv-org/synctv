use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosCredential {
    pub username: String,
    pub password: String,
    pub token: String,
    pub long_token: Option<String>,
    pub secret: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FnosLogin {
    Authenticated(FnosCredential),
    Challenge(FnosLoginChallenge),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosLoginChallenge {
    pub access_token: String,
    pub setup_required: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosServerInfo {
    pub host_name: String,
    pub version: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosFileList {
    pub files: Vec<FnosFile>,
    pub revision: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosFile {
    pub name: String,
    pub path: String,
    pub size: Option<u64>,
    pub modified_at: Option<i64>,
    pub created_at: Option<i64>,
    pub is_dir: bool,
    pub storage_id: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FnosWebDavConfig {
    pub enabled: bool,
    pub endpoint: Option<String>,
    pub root: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct FileListResponse {
    #[serde(default)]
    pub files: Vec<RawFile>,
    pub uver: Option<u64>,
    pub result: Option<String>,
    pub errno: Option<i64>,
    pub msg: Option<String>,
    pub errmsg: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct RawFile {
    #[serde(default)]
    pub name: String,
    /// FNOS returns the owning user id for entries in the user's root.
    /// It is required together with `v` to address child directories.
    pub uid: Option<u64>,
    pub size: Option<u64>,
    pub mtim: Option<i64>,
    pub btim: Option<i64>,
    pub dir: Option<u8>,
    pub v: Option<u64>,
}
