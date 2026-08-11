use serde::Deserialize;

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub(crate) struct AuthTokenResponse {
    pub token: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SeafileServerInfo {
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub features: Vec<String>,
    #[serde(default)]
    pub encrypted_library_version: u32,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SeafileAccount {
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub login_id: String,
    #[serde(default)]
    pub contact_email: String,
    #[serde(default)]
    pub total: i64,
    #[serde(default)]
    pub usage: i64,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SeafileRepository {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub owner: String,
    #[serde(default)]
    pub owner_name: String,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub encrypted: bool,
    #[serde(default)]
    pub password_need: bool,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: i64,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct DirectoryItem {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: i64,
    #[serde(default)]
    pub permission: String,
    #[serde(default)]
    pub modifier_name: String,
    #[serde(default)]
    pub starred: bool,
    #[serde(default)]
    pub encoded_thumbnail_src: String,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(untagged)]
pub(crate) enum DirectoryResponse {
    Items(Vec<DirectoryItem>),
    Envelope {
        #[serde(default)]
        dirent_list: Vec<DirectoryItem>,
    },
}

impl DirectoryResponse {
    pub(crate) fn into_items(self) -> Vec<DirectoryItem> {
        match self {
            Self::Items(items) => items,
            Self::Envelope { dirent_list } => dirent_list,
        }
    }
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SearchResponse {
    #[serde(default)]
    pub data: Vec<SearchItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct SearchItem {
    #[serde(default, rename = "type")]
    pub kind: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default)]
    pub mtime: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StarredResponse {
    #[serde(default)]
    pub starred_item_list: Vec<StarredItem>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct StarredItem {
    #[serde(default)]
    pub repo_id: String,
    #[serde(default)]
    pub repo_name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub obj_name: String,
    #[serde(default)]
    pub is_dir: bool,
    #[serde(default)]
    pub deleted: bool,
    #[serde(default)]
    pub mtime: String,
    #[serde(default)]
    pub encoded_thumbnail_src: String,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SeafileItem {
    pub repository_id: String,
    pub repository_name: String,
    pub path: String,
    pub name: String,
    pub object_id: String,
    pub is_directory: bool,
    pub size: u64,
    pub modified_at: String,
    pub permission: String,
    pub modifier_name: String,
    pub starred: bool,
    pub has_thumbnail: bool,
    pub repository_encrypted: bool,
    pub password_required: bool,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct SeafileFileInfo {
    #[serde(default)]
    pub repo_id: String,
    #[serde(default)]
    pub parent_dir: String,
    #[serde(default, rename = "obj_name")]
    pub name: String,
    #[serde(default, rename = "obj_id")]
    pub object_id: String,
    #[serde(default)]
    pub size: u64,
    #[serde(default, rename = "mtime")]
    pub modified_at: String,
    #[serde(default)]
    pub is_locked: bool,
    #[serde(default)]
    pub can_preview: bool,
    #[serde(default)]
    pub can_edit: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SeafileList {
    pub items: Vec<SeafileItem>,
    pub total: u64,
    pub page: u64,
    pub page_size: u32,
}
