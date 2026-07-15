use serde::Deserialize;

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OcsEnvelope<T> {
    pub ocs: OcsBody<T>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct OcsBody<T> {
    pub meta: NextcloudOcsMeta,
    pub data: T,
}

#[derive(Debug, Clone, Deserialize)]
pub struct NextcloudOcsMeta {
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub statuscode: i64,
    #[serde(default)]
    pub message: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NextcloudUser {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub displayname: String,
    #[serde(default)]
    pub email: Option<String>,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct CapabilitiesData {
    pub version: NextcloudVersion,
    #[serde(default)]
    pub capabilities: serde_json::Value,
}

#[derive(Debug, Clone, Deserialize)]
pub(crate) struct NextcloudVersion {
    #[serde(default)]
    pub major: u32,
    #[serde(default)]
    pub minor: u32,
    #[serde(default)]
    pub micro: u32,
    #[serde(default)]
    pub string: String,
    #[serde(default)]
    pub edition: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextcloudCapabilities {
    pub version_major: u32,
    pub version_minor: u32,
    pub version_micro: u32,
    pub version: String,
    pub edition: String,
    pub values: serde_json::Value,
}

impl From<CapabilitiesData> for NextcloudCapabilities {
    fn from(value: CapabilitiesData) -> Self {
        Self {
            version_major: value.version.major,
            version_minor: value.version.minor,
            version_micro: value.version.micro,
            version: value.version.string,
            edition: value.version.edition,
            values: value.capabilities,
        }
    }
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NextcloudLoginFlow {
    pub poll: NextcloudLoginFlowPoll,
    pub login: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
pub struct NextcloudLoginFlowPoll {
    pub token: String,
    pub endpoint: String,
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "camelCase")]
pub struct NextcloudLoginFlowCredentials {
    pub server: String,
    pub login_name: String,
    pub app_password: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextcloudServerInfo {
    pub user: NextcloudUser,
    pub capabilities: NextcloudCapabilities,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct NextcloudDavItem {
    pub href: String,
    pub path: String,
    pub name: String,
    pub file_id: u64,
    pub size: u64,
    pub modified_at: Option<String>,
    pub content_type: Option<String>,
    pub etag: Option<String>,
    pub permissions: Option<String>,
    pub owner_id: Option<String>,
    pub owner_display_name: Option<String>,
    pub favorite: bool,
    pub has_preview: bool,
    pub is_directory: bool,
    pub blurhash: Option<String>,
    pub width: Option<u32>,
    pub height: Option<u32>,
    pub duration_millis: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NextcloudList {
    pub items: Vec<NextcloudDavItem>,
    pub total: Option<u64>,
    pub page: u64,
    pub page_size: u32,
    pub has_more: bool,
}
