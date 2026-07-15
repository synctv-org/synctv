use serde::Deserialize;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctvResource {
    pub page_url: Option<String>,
    pub video_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CctvMedia {
    pub metadata: CctvMetadata,
    pub playback: CctvPlayback,
}

#[derive(Debug, Clone, PartialEq)]
pub struct CctvMetadata {
    pub video_id: String,
    pub title: String,
    pub description: Option<String>,
    pub uploader: Option<String>,
    pub producer: Option<String>,
    pub channel: Option<String>,
    pub column: Option<String>,
    pub tags: Vec<String>,
    pub thumbnail_url: Option<String>,
    pub duration_seconds: Option<f64>,
    pub published_at: Option<i64>,
    pub chapters: Vec<CctvChapter>,
    pub protected: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctvChapter {
    pub id: String,
    pub title: String,
    pub start_ms: u64,
    pub end_ms: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CctvStreamKind {
    VideoHls,
    AudioHls,
    Http,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctvStream {
    pub name: String,
    pub url: String,
    pub kind: CctvStreamKind,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CctvPlayback {
    pub video_id: String,
    pub streams: Vec<CctvStream>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VideoInfoResponse {
    #[serde(default)]
    pub title: String,
    pub ack: Option<String>,
    pub status: Option<String>,
    pub tag: Option<String>,
    pub play_channel: Option<String>,
    pub produce: Option<String>,
    pub editer_name: Option<String>,
    pub column: Option<String>,
    pub f_pgmtime: Option<String>,
    pub image: Option<String>,
    pub video: Option<VideoInfo>,
    pub hls_url: Option<String>,
    pub manifest: Option<Manifest>,
    #[serde(default)]
    pub segments: Vec<Segment>,
    pub is_invalid_copyright: Option<String>,
    pub is_protected: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub(crate) struct VideoInfo {
    pub total_length: Option<String>,
    #[serde(default)]
    pub low_chapters: Vec<VideoFile>,
    #[serde(default)]
    pub chapters: Vec<VideoFile>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct VideoFile {
    #[serde(default)]
    pub url: String,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Manifest {
    pub audio_mp3: Option<String>,
    pub hls_audio_url: Option<String>,
}

#[derive(Debug, Deserialize)]
pub(crate) struct Segment {
    #[serde(default)]
    pub guid: String,
    #[serde(default)]
    pub title: String,
    pub start: Option<u64>,
    pub end: Option<u64>,
}
