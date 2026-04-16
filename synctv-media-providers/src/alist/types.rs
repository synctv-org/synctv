//! Alist HTTP API Types
//!
//! HTTP-specific types for Alist JSON API responses.
//! These are converted to proto types in the service layer.

use serde::Deserialize;

/// Generic Alist API response wrapper (for HTTP responses)
#[derive(Debug, Deserialize)]
pub struct AlistResp<T> {
    pub code: i64,
    pub message: String,
    pub data: Option<T>,
}

/// Login response data (for HTTP API)
#[derive(Debug, Deserialize)]
pub struct LoginData {
    pub token: String,
}

/// File/folder information from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpFsGetResp {
    pub name: String,
    pub size: u64,
    #[serde(rename = "is_dir")]
    pub is_dir: bool,
    #[serde(default)]
    pub modified: u64,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub sign: String,
    #[serde(default)]
    pub thumb: String,
    #[serde(rename = "type", default)]
    pub r#type: u64,
    #[serde(default)]
    pub hashinfo: String,
    #[serde(default)]
    pub raw_url: String,
    #[serde(default)]
    pub readme: String,
    #[serde(default)]
    pub provider: String,
    #[serde(default)]
    pub related: Vec<HttpFsGetRelated>,
}

#[derive(Debug, Deserialize)]
pub struct HttpFsGetRelated {
    pub name: String,
    pub size: u64,
    #[serde(rename = "is_dir")]
    pub is_dir: bool,
    #[serde(default)]
    pub modified: u64,
    #[serde(default)]
    pub created: u64,
    #[serde(default)]
    pub sign: String,
    #[serde(default)]
    pub thumb: String,
    #[serde(rename = "type", default)]
    pub r#type: u64,
    #[serde(default)]
    pub hashinfo: String,
}

/// Directory listing from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpFsListResp {
    pub content: Vec<HttpFsListContent>,
    pub total: u64,
    #[serde(default)]
    pub readme: String,
    #[serde(default)]
    pub write: bool,
    #[serde(default)]
    pub provider: String,
}

#[derive(Debug, Deserialize)]
pub struct HttpFsListContent {
    pub name: String,
    pub size: u64,
    #[serde(rename = "is_dir")]
    pub is_dir: bool,
    #[serde(default)]
    pub modified: u64,
    #[serde(default)]
    pub sign: String,
    #[serde(default)]
    pub thumb: String,
    #[serde(rename = "type", default)]
    pub r#type: u64,
}

/// Me response from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpMeResp {
    pub id: u64,
    pub username: String,
    #[serde(default, skip)]
    _password: String, // never expose password hash
    #[serde(rename = "base_path")]
    pub base_path: String,
    pub role: u64,
    pub disabled: bool,
    pub permission: u64,
    #[serde(rename = "sso_id")]
    pub sso_id: String,
    pub otp: bool,
}

/// Search content item from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpFsSearchContent {
    pub parent: String,
    pub name: String,
    #[serde(rename = "is_dir")]
    pub is_dir: bool,
    pub size: u64,
    #[serde(rename = "type")]
    pub r#type: u64,
}

/// Search response from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpFsSearchResp {
    pub content: Vec<HttpFsSearchContent>,
    pub total: u64,
}

/// Video preview information from HTTP API
#[derive(Debug, Deserialize)]
pub struct HttpFsOtherResp {
    #[serde(default)]
    pub drive_id: String,
    #[serde(default)]
    pub file_id: String,
    pub video_preview_play_info: Option<HttpVideoPreviewPlayInfo>,
}

#[derive(Debug, Deserialize)]
pub struct HttpVideoPreviewPlayInfo {
    #[serde(default)]
    pub category: String,
    #[serde(default)]
    pub live_transcoding_subtitle_task_list: Vec<HttpSubtitleTask>,
    #[serde(default)]
    pub live_transcoding_task_list: Vec<HttpTranscodingTask>,
    pub meta: Option<HttpVideoMeta>,
}

#[derive(Debug, Deserialize)]
pub struct HttpTranscodingTask {
    #[serde(default)]
    pub stage: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub template_height: u64,
    #[serde(default)]
    pub template_id: String,
    #[serde(default)]
    pub template_name: String,
    #[serde(default)]
    pub template_width: u64,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct HttpSubtitleTask {
    #[serde(default)]
    pub language: String,
    #[serde(default)]
    pub status: String,
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub struct HttpVideoMeta {
    #[serde(default)]
    pub duration: f64,
    #[serde(default)]
    pub height: u64,
    #[serde(default)]
    pub width: u64,
}

// From trait implementations for proto conversion

impl From<HttpFsGetRelated> for crate::grpc::alist::fs_get_resp::FsGetRelated {
    fn from(related: HttpFsGetRelated) -> Self {
        Self {
            name: related.name,
            size: related.size,
            is_dir: related.is_dir,
            modified: related.modified,
            created: related.created,
            sign: related.sign,
            thumb: related.thumb,
            r#type: related.r#type,
            hashinfo: related.hashinfo,
        }
    }
}

impl From<HttpFsGetResp> for crate::grpc::alist::FsGetResp {
    fn from(resp: HttpFsGetResp) -> Self {
        Self {
            name: resp.name,
            size: resp.size,
            is_dir: resp.is_dir,
            modified: resp.modified,
            created: resp.created,
            sign: resp.sign,
            thumb: resp.thumb,
            r#type: resp.r#type,
            hashinfo: resp.hashinfo,
            raw_url: resp.raw_url,
            readme: resp.readme,
            provider: resp.provider,
            related: resp
                .related
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
        }
    }
}

impl From<HttpFsListContent> for crate::grpc::alist::fs_list_resp::FsListContent {
    fn from(item: HttpFsListContent) -> Self {
        Self {
            name: item.name,
            size: item.size,
            is_dir: item.is_dir,
            modified: item.modified,
            sign: item.sign,
            thumb: item.thumb,
            r#type: item.r#type,
        }
    }
}

impl From<HttpFsListResp> for crate::grpc::alist::FsListResp {
    fn from(resp: HttpFsListResp) -> Self {
        Self {
            content: resp
                .content
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            total: resp.total,
            readme: resp.readme,
            write: resp.write,
            provider: resp.provider,
        }
    }
}

impl From<HttpVideoMeta> for crate::grpc::alist::fs_other_resp::video_preview_play_info::Meta {
    fn from(meta: HttpVideoMeta) -> Self {
        Self {
            duration: meta.duration,
            height: meta.height,
            width: meta.width,
        }
    }
}

impl From<HttpSubtitleTask>
    for crate::grpc::alist::fs_other_resp::video_preview_play_info::LiveTranscodingSubtitleTaskList
{
    fn from(sub: HttpSubtitleTask) -> Self {
        Self {
            language: sub.language,
            status: sub.status,
            url: sub.url,
        }
    }
}

impl From<HttpTranscodingTask>
    for crate::grpc::alist::fs_other_resp::video_preview_play_info::LiveTranscodingTaskList
{
    fn from(task: HttpTranscodingTask) -> Self {
        Self {
            stage: task.stage,
            status: task.status,
            template_height: task.template_height,
            template_id: task.template_id,
            template_name: task.template_name,
            template_width: task.template_width,
            url: task.url,
        }
    }
}

impl From<HttpVideoPreviewPlayInfo> for crate::grpc::alist::fs_other_resp::VideoPreviewPlayInfo {
    fn from(preview: HttpVideoPreviewPlayInfo) -> Self {
        Self {
            category: preview.category,
            live_transcoding_subtitle_task_list: preview
                .live_transcoding_subtitle_task_list
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            live_transcoding_task_list: preview
                .live_transcoding_task_list
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            meta: preview.meta.map(std::convert::Into::into),
        }
    }
}

impl From<HttpFsOtherResp> for crate::grpc::alist::FsOtherResp {
    fn from(resp: HttpFsOtherResp) -> Self {
        Self {
            drive_id: resp.drive_id,
            file_id: resp.file_id,
            video_preview_play_info: resp.video_preview_play_info.map(std::convert::Into::into),
        }
    }
}

impl From<HttpMeResp> for crate::grpc::alist::MeResp {
    fn from(resp: HttpMeResp) -> Self {
        Self {
            id: resp.id,
            username: resp.username,
            base_path: resp.base_path,
            role: resp.role,
            disabled: resp.disabled,
            permission: resp.permission,
            sso_id: resp.sso_id,
            otp: resp.otp,
        }
    }
}

impl From<HttpFsSearchContent> for crate::grpc::alist::fs_search_resp::FsSearchContent {
    fn from(item: HttpFsSearchContent) -> Self {
        Self {
            parent: item.parent,
            name: item.name,
            is_dir: item.is_dir,
            size: item.size,
            r#type: item.r#type,
        }
    }
}

impl From<HttpFsSearchResp> for crate::grpc::alist::FsSearchResp {
    fn from(resp: HttpFsSearchResp) -> Self {
        Self {
            content: resp
                .content
                .into_iter()
                .map(std::convert::Into::into)
                .collect(),
            total: resp.total,
        }
    }
}
