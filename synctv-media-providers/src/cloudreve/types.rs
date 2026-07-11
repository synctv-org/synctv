use serde::Deserialize;

#[derive(Debug, Deserialize)]
pub(crate) struct CloudreveResponse<T> {
    pub code: i64,
    #[serde(default, alias = "message")]
    pub msg: String,
    pub data: Option<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveToken {
    pub access_token: String,
    #[serde(default)]
    pub refresh_token: String,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CloudreveLogin {
    pub token: CloudreveToken,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveUser {
    pub id: String,
    #[serde(default)]
    pub email: String,
    #[serde(default)]
    pub nickname: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveFile {
    pub id: String,
    pub name: String,
    #[serde(default)]
    pub path: String,
    #[serde(default)]
    pub size: i64,
    #[serde(rename = "type")]
    pub file_type: i64,
    #[serde(default)]
    pub updated_at: Option<chrono::DateTime<chrono::Utc>>,
    #[serde(default)]
    pub metadata: std::collections::HashMap<String, String>,
    #[serde(skip)]
    pub thumbnail_url: Option<String>,
}

impl CloudreveFile {
    #[must_use]
    pub const fn is_dir(&self) -> bool {
        self.file_type == 1
    }

    #[must_use]
    pub fn thumbnail(&self) -> String {
        self.thumbnail_url.clone().unwrap_or_else(|| {
            self.metadata
                .get("thumb")
                .or_else(|| self.metadata.get("thumbnail"))
                .cloned()
                .unwrap_or_default()
        })
    }
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudrevePagination {
    #[serde(default)]
    pub total_items: u64,
    #[serde(default)]
    pub next_token: String,
    #[serde(default)]
    pub is_cursor: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveList {
    #[serde(default)]
    pub files: Vec<CloudreveFile>,
    pub pagination: Option<CloudrevePagination>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveSearchHit {
    pub file: CloudreveFile,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveSearch {
    #[serde(default)]
    pub hits: Vec<CloudreveSearchHit>,
    #[serde(default)]
    pub total: u64,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveEntityUrl {
    #[serde(alias = "Url", alias = "URL")]
    pub url: String,
    #[serde(default, alias = "ExpireAt")]
    pub expire_at: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveUrl {
    #[serde(default)]
    pub urls: Vec<CloudreveEntityUrl>,
    #[serde(default)]
    pub expires: Option<chrono::DateTime<chrono::Utc>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct CloudreveThumbnail {
    #[serde(alias = "Url", alias = "URL")]
    pub url: String,
    #[serde(default)]
    pub expires: Option<chrono::DateTime<chrono::Utc>>,
}
