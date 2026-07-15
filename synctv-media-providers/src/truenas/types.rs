use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TrueNasSystemInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub hostname: String,
    #[serde(default)]
    pub system_product: Option<String>,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct TrueNasFileItem {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub realpath: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub allocation_size: u64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
    #[serde(default)]
    pub mount_id: u64,
    #[serde(default)]
    pub acl: bool,
    #[serde(default)]
    pub is_mountpoint: bool,
    #[serde(default)]
    pub is_ctldir: bool,
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub xattrs: Vec<String>,
    #[serde(default)]
    pub zfs_attrs: Option<Vec<String>>,
}

#[derive(Debug, Clone, Deserialize, PartialEq)]
pub struct TrueNasFileStat {
    #[serde(default)]
    pub realpath: String,
    #[serde(rename = "type", default)]
    pub kind: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub allocation_size: u64,
    #[serde(default)]
    pub mode: u32,
    #[serde(default)]
    pub mount_id: u64,
    #[serde(default)]
    pub uid: u32,
    #[serde(default)]
    pub gid: u32,
    #[serde(default)]
    pub atime: f64,
    #[serde(default)]
    pub mtime: f64,
    #[serde(default)]
    pub ctime: f64,
    #[serde(default)]
    pub btime: f64,
    #[serde(default)]
    pub dev: u64,
    #[serde(default)]
    pub inode: u64,
    #[serde(default)]
    pub nlink: u64,
    #[serde(default)]
    pub acl: bool,
    #[serde(default)]
    pub is_mountpoint: bool,
    #[serde(default)]
    pub is_ctldir: bool,
    #[serde(default)]
    pub attributes: Vec<String>,
    #[serde(default)]
    pub user: Option<String>,
    #[serde(default)]
    pub group: Option<String>,
}

impl TrueNasFileItem {
    #[must_use]
    pub fn is_directory(&self) -> bool {
        self.kind == "DIRECTORY"
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrueNasList {
    pub items: Vec<TrueNasFileItem>,
    pub total: u64,
    pub page: u64,
    pub page_size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TrueNasDownloadTicket {
    pub job_id: u64,
    pub url: url::Url,
}
